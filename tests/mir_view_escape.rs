//! A view must not outlive what it views by riding in something the
//! borrow check does not follow (core semantics §5.7, §9.3): a closure
//! stored in a struct field, an enum payload, a tuple inside a struct or a
//! collection, whether it captured a local view or a view parameter, or a
//! view-holding value cast to `dyn`, which owns its value and keeps no
//! borrow (B-2026-11-17-23, -24, -25). Each refused program below but the
//! last is a cell of the origin-tracking probe that `karac check` used to
//! accept and that then read freed memory under the MIR interpreter and
//! ASan. The accepted controls keep a view where the check can follow it,
//! or capture only owned values.

use std::process::Command;

/// `karac check` on `src`: its success and its output.
fn check(name: &str, src: &str) -> (bool, String) {
    let dir = std::env::temp_dir().join(format!("karac-view-escape-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.kara");
    std::fs::write(&file, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg("check")
        .arg(&file)
        .output()
        .unwrap();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let _ = std::fs::remove_dir_all(&dir);
    (out.status.success(), text)
}

const D: &str = "struct D { n: i64 }\nimpl Drop for D { fn drop(mut ref self) { println(f\"drop {self.n}\"); } }\n";

/// The probe cells, then a local `dyn` of a view: name, the code `check`
/// refuses with, the program after [`D`].
const REFUSED: &[(&str, &str, &str)] = &[
    (
        "closure.enum.fn",
        "E0509",
        r#"enum E1 { A(Fn() -> i64), B }
fn mk(v: Slice[D]) -> E1 { E1.A(|| v[0].n) }
fn main() { let r = { let x: Vec[D] = [D { n: 1 }, D { n: 2 }]; mk(x[0..2]) }; println("after"); println(f"{match r { E1.A(q) => (q)(), E1.B => 0 }}"); }
"#,
    ),
    (
        "closure.enum.id",
        "E0509",
        r#"enum E1 { A(Fn() -> i64), B }
fn mk(v: Slice[D]) -> E1 { E1.A(|| v[0].n) }
fn id[T](t: own T) -> T { t }
fn main() { let r = { let x: Vec[D] = [D { n: 1 }, D { n: 2 }]; id(mk(x[0..2])) }; println("after"); println(f"{match r { E1.A(q) => (q)(), E1.B => 0 }}"); }
"#,
    ),
    (
        "closure.enum.self",
        "E0509",
        r#"enum E1 { A(Fn() -> i64), B }
impl E1 { fn me(own self) -> Self { self } }
fn mk(v: Slice[D]) -> E1 { E1.A(|| v[0].n) }
fn main() { let r = { let x: Vec[D] = [D { n: 1 }, D { n: 2 }]; mk(x[0..2]).me() }; println("after"); println(f"{match r { E1.A(q) => (q)(), E1.B => 0 }}"); }
"#,
    ),
    (
        "closure.nested.fn",
        "E0509",
        r#"struct S1 { a: Fn() -> i64 }
struct S2 { s: S1, k: i64 }
fn mk(v: Slice[D]) -> S2 { S2 { s: S1 { a: || v[0].n }, k: 0 } }
fn main() { let r = { let x: Vec[D] = [D { n: 1 }, D { n: 2 }]; mk(x[0..2]) }; println("after"); println(f"{(r.s.a)()}"); }
"#,
    ),
    (
        "closure.nested.id",
        "E0509",
        r#"struct S1 { a: Fn() -> i64 }
struct S2 { s: S1, k: i64 }
fn mk(v: Slice[D]) -> S2 { S2 { s: S1 { a: || v[0].n }, k: 0 } }
fn id[T](t: own T) -> T { t }
fn main() { let r = { let x: Vec[D] = [D { n: 1 }, D { n: 2 }]; id(mk(x[0..2])) }; println("after"); println(f"{(r.s.a)()}"); }
"#,
    ),
    (
        "closure.nested.self",
        "E0509",
        r#"struct S1 { a: Fn() -> i64 }
struct S2 { s: S1, k: i64 }
impl S2 { fn me(own self) -> Self { self } }
fn mk(v: Slice[D]) -> S2 { S2 { s: S1 { a: || v[0].n }, k: 0 } }
fn main() { let r = { let x: Vec[D] = [D { n: 1 }, D { n: 2 }]; mk(x[0..2]).me() }; println("after"); println(f"{(r.s.a)()}"); }
"#,
    ),
    (
        "closure.struct.fn",
        "E0509",
        r#"struct S1 { a: Fn() -> i64 }
fn mk(v: Slice[D]) -> S1 { S1 { a: || v[0].n } }
fn main() { let r = { let x: Vec[D] = [D { n: 1 }, D { n: 2 }]; mk(x[0..2]) }; println("after"); println(f"{(r.a)()}"); }
"#,
    ),
    (
        "closure.struct.id",
        "E0509",
        r#"struct S1 { a: Fn() -> i64 }
fn mk(v: Slice[D]) -> S1 { S1 { a: || v[0].n } }
fn id[T](t: own T) -> T { t }
fn main() { let r = { let x: Vec[D] = [D { n: 1 }, D { n: 2 }]; id(mk(x[0..2])) }; println("after"); println(f"{(r.a)()}"); }
"#,
    ),
    (
        "closure.struct.method_builder",
        "E0509",
        r#"struct Bld { fs: Vec[Fn() -> i64] }
impl Bld { fn add(own self, f: escaping Fn() -> i64) -> Self { let mut b = self; b.fs.push(f); b } }
fn main() { let r = { let x: Vec[D] = [D { n: 1 }, D { n: 2 }]; let v = x[0..2]; Bld { fs: Vec.new() }.add(|| v[0].n) }; println("after"); println(f"{(r.fs[0])()}"); }
"#,
    ),
    (
        "closure.struct.mutref_push",
        "E0509",
        r#"struct Bag { fs: Vec[Fn() -> i64] }
fn put(b: mut ref Bag, v: Slice[D]) { b.fs.push(|| v[0].n); }
fn main() { let mut bag = Bag { fs: Vec.new() }; { let x: Vec[D] = [D { n: 1 }, D { n: 2 }]; put(mut bag, x[0..2]); } println("after"); println(f"{(bag.fs[0])()}"); }
"#,
    ),
    (
        "closure.struct.self",
        "E0509",
        r#"struct S1 { a: Fn() -> i64 }
impl S1 { fn me(own self) -> Self { self } }
fn mk(v: Slice[D]) -> S1 { S1 { a: || v[0].n } }
fn main() { let r = { let x: Vec[D] = [D { n: 1 }, D { n: 2 }]; mk(x[0..2]).me() }; println("after"); println(f"{(r.a)()}"); }
"#,
    ),
    (
        "closure.tuple_in_struct.fn",
        "E0509",
        r#"struct S3 { t: (Fn() -> i64, i64) }
fn mk(v: Slice[D]) -> S3 { S3 { t: (|| v[0].n, 0) } }
fn main() { let r = { let x: Vec[D] = [D { n: 1 }, D { n: 2 }]; mk(x[0..2]) }; println("after"); println(f"{(r.t.0)()}"); }
"#,
    ),
    (
        "closure.tuple_in_struct.id",
        "E0509",
        r#"struct S3 { t: (Fn() -> i64, i64) }
fn mk(v: Slice[D]) -> S3 { S3 { t: (|| v[0].n, 0) } }
fn id[T](t: own T) -> T { t }
fn main() { let r = { let x: Vec[D] = [D { n: 1 }, D { n: 2 }]; id(mk(x[0..2])) }; println("after"); println(f"{(r.t.0)()}"); }
"#,
    ),
    (
        "closure.tuple_in_struct.self",
        "E0509",
        r#"struct S3 { t: (Fn() -> i64, i64) }
impl S3 { fn me(own self) -> Self { self } }
fn mk(v: Slice[D]) -> S3 { S3 { t: (|| v[0].n, 0) } }
fn main() { let r = { let x: Vec[D] = [D { n: 1 }, D { n: 2 }]; mk(x[0..2]).me() }; println("after"); println(f"{(r.t.0)()}"); }
"#,
    ),
    (
        "closure.vec.block",
        "E0509",
        r#"fn main() { let r = { let x: Vec[D] = [D { n: 1 }, D { n: 2 }]; let v = x[0..2]; [|| v[0].n] }; println("after"); println(f"{(r[0])()}"); }
"#,
    ),
    (
        "closure.vec.fn",
        "E0509",
        r#"fn mk(v: Slice[D]) -> Vec[Fn() -> i64] { [|| v[0].n] }
fn main() { let r = { let x: Vec[D] = [D { n: 1 }, D { n: 2 }]; mk(x[0..2]) }; println("after"); println(f"{(r[0])()}"); }
"#,
    ),
    (
        "closure.vec.generic",
        "E0509",
        r#"struct G[T] { g: T }
fn mkg[T](t: own T) -> G[T] { G { g: t } }
fn main() { let r = { let x: Vec[D] = [D { n: 1 }, D { n: 2 }]; let v = x[0..2]; mkg([|| v[0].n]) }; println("after"); println(f"{(r.g[0])()}"); }
"#,
    ),
    (
        "closure.vec.id",
        "E0509",
        r#"fn mk(v: Slice[D]) -> Vec[Fn() -> i64] { [|| v[0].n] }
fn id[T](t: own T) -> T { t }
fn main() { let r = { let x: Vec[D] = [D { n: 1 }, D { n: 2 }]; id(mk(x[0..2])) }; println("after"); println(f"{(r[0])()}"); }
"#,
    ),
    (
        "dyn.closure_field.fn",
        "E0509",
        r#"trait Get { fn get(self) -> i64; }
struct Ch { f: Fn() -> i64 }
impl Get for Ch { fn get(self) -> i64 { (self.f)() } }
fn mk(v: Slice[D]) -> dyn Get { Ch { f: || v[0].n } }
fn main() { let r = { let x: Vec[D] = [D { n: 1 }, D { n: 2 }]; mk(x[0..2]) }; println("after"); println(f"{r.get()}"); }
"#,
    ),
    (
        "dyn.owned.block",
        "E0509",
        r#"trait Get { fn get(self) -> i64; }
struct Vh { v: Slice[D] }
impl Get for Vh { fn get(self) -> i64 { self.v[0].n } }
fn main() { let r: dyn Get = { let x: Vec[D] = [D { n: 1 }, D { n: 2 }]; Vh { v: x[0..2] } }; println("after"); println(f"{r.get()}"); }
"#,
    ),
    (
        "dyn.owned.fn",
        "E0509",
        r#"trait Get { fn get(self) -> i64; }
struct Vh { v: Slice[D] }
impl Get for Vh { fn get(self) -> i64 { self.v[0].n } }
fn mk(v: Slice[D]) -> dyn Get { Vh { v: v } }
fn main() { let r = { let x: Vec[D] = [D { n: 1 }, D { n: 2 }]; mk(x[0..2]) }; println("after"); println(f"{r.get()}"); }
"#,
    ),
    (
        "dyn.struct.fn",
        "E0509",
        r#"trait Get { fn get(self) -> i64; }
struct Vh { v: Slice[D] }
impl Get for Vh { fn get(self) -> i64 { self.v[0].n } }
struct Hd { h: dyn Get }
fn mk(v: Slice[D]) -> Hd { Hd { h: Vh { v: v } } }
fn main() { let r = { let x: Vec[D] = [D { n: 1 }, D { n: 2 }]; mk(x[0..2]) }; println("after"); println(f"{r.h.get()}"); }
"#,
    ),
    (
        "dyn.vec.fn",
        "E0509",
        r#"trait Get { fn get(self) -> i64; }
struct Vh { v: Slice[D] }
impl Get for Vh { fn get(self) -> i64 { self.v[0].n } }
fn mk(v: Slice[D]) -> Vec[dyn Get] { [Vh { v: v }] }
fn main() { let r = { let x: Vec[D] = [D { n: 1 }, D { n: 2 }]; mk(x[0..2]) }; println("after"); println(f"{r[0].get()}"); }
"#,
    ),
    (
        "dyn.local_view",
        "E0509",
        r#"trait Get { fn get(self) -> i64; }
struct Vh { v: Slice[i64] }
impl Get for Vh { fn get(self) -> i64 { self.v[0] } }
fn main() { let x: Vec[i64] = [9, 8]; let r: dyn Get = Vh { v: x[0..2] }; println(f"{r.get()}"); }
"#,
    ),
];

#[test]
fn view_escape_cells_are_refused() {
    let mut wrong = Vec::new();
    for (name, code, body) in REFUSED {
        let (ok, text) = check(name, &format!("{D}{body}"));
        if ok || !text.contains(&format!("[{code}]")) {
            wrong.push(format!("{name}: expected [{code}], got: {text}"));
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
    assert_eq!(REFUSED.len(), 24);
}

/// Views the check follows, and stored closures and `dyn` values that hold
/// only owned values.
const ACCEPTED: &[(&str, &str)] = &[
    (
        "c1",
        r#"struct D { n: i64 }
impl Drop for D { fn drop(mut ref self) { println(f"drop {self.n}"); } }
fn mk(v: Slice[D]) -> Fn() -> i64 { || v[0].n + v[1].n }
fn main() { let x: Vec[D] = [D { n: 1 }, D { n: 2 }]; let f = mk(x[0..2]); println(f"{f()}"); }
"#,
    ),
    (
        "c2",
        r#"struct H { f: Fn() -> i64 }
fn mk(v: Slice[i64]) -> H { let k = v[0] + v[1]; H { f: || k * 2 } }
fn main() { let h = { let x: Vec[i64] = [3, 4]; mk(x[0..2]) }; println(f"{(h.f)()}"); }
"#,
    ),
    (
        "c3",
        r#"trait Get { fn get(self) -> i64; }
struct Own { v: Vec[i64] }
impl Get for Own { fn get(self) -> i64 { self.v[0] } }
struct Hd { h: dyn Get }
fn mk(v: Slice[i64]) -> Hd { Hd { h: Own { v: v.to_vec() } } }
fn main() { let r = { let x: Vec[i64] = [5, 6]; mk(x[0..2]) }; let vs: Vec[dyn Get] = [Own { v: [7] }]; println(f"{r.h.get()} {vs[0].get()}"); }
"#,
    ),
    (
        "c4",
        r#"struct Bld { fs: Vec[Fn() -> i64] }
impl Bld { fn add(own self, f: escaping Fn() -> i64) -> Self { let mut b = self; b.fs.push(f); b } }
fn main() { let r = { let x: Vec[i64] = [1, 2]; let n = x[1]; Bld { fs: Vec.new() }.add(|| n * 10) }; println(f"{(r.fs[0])()}"); }
"#,
    ),
    (
        "c5",
        r#"fn total(v: Slice[i64], k: i64) -> Vec[i64] { (0..k).map(|i| v[i] * 2).collect() }
fn label(s: Str) -> String { let f = || s.len(); f().to_string() }
fn main() { let x: Vec[i64] = [1, 2, 3]; println(f"{total(x[0..3], 2)} {label("abc")}"); }
"#,
    ),
    (
        "c7",
        r#"import std.sync.{TaskGroup};
fn main() { let x: Vec[i64] = [1, 2, 3]; let s = x[0..3]; let g = TaskGroup.new(); let h = g.spawn(|| s[0] + s[2]); drop(g); println(f"{h.join()}"); }
"#,
    ),
];

#[test]
fn view_escape_controls_are_accepted() {
    let mut wrong = Vec::new();
    for (name, src) in ACCEPTED {
        let (ok, text) = check(name, src);
        if !ok {
            wrong.push(format!("{name}: {text}"));
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
    assert_eq!(ACCEPTED.len(), 6);
}

/// The message names the parameter whose view escapes.
#[test]
fn view_param_escape_names_the_parameter() {
    let (ok, text) = check(
        "names",
        &format!("{D}struct H {{ f: Fn() -> i64 }}\nfn mk(v: Slice[D]) -> H {{ H {{ f: || v[0].n }} }}\nfn main() {{ let x: Vec[D] = [D {{ n: 1 }}]; let h = mk(x[0..1]); println(f\"{{(h.f)()}}\"); }}\n"),
    );
    assert!(!ok);
    assert!(
        text.contains("a view the parameter `v` holds is stored in an escaping closure"),
        "{text}"
    );
}
