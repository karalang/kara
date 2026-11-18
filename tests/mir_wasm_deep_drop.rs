//! B-2026-11-16-311: dropping a deep `shared` tree (two or more handles of
//! its own type, `left` and `right`) recursed once per level, and on
//! wasm_wasi node's engine stack ended a 20000-deep one ("Maximum call stack
//! size exceeded"). The release now keeps the boxes still to release on a
//! stack held in their own handle slots, so the depth costs no frames, and
//! `Drop` bodies run in the order the recursive release ran them.
//!
//! The wasm half skips (with a notice) when the wasm archive or node is
//! missing, as `tests/mir_wasm.rs` does.
#![cfg(feature = "llvm")]

use std::path::{Path, PathBuf};
use std::process::Command;

const RUNNER: &str = "\
import { readFile } from 'node:fs/promises';
import { WASI } from 'node:wasi';
const wasi = new WASI({ version: 'preview1', args: [process.argv[2]], env: {}, returnOnExit: true });
const mod = await WebAssembly.compile(await readFile(process.argv[2]));
const inst = await WebAssembly.instantiate(mod, wasi.getImportObject());
process.exitCode = wasi.start(inst);
";

/// A left chain of 200000 two-child nodes, dropped at the end of `main`.
const DEEP: &str = r#"shared struct T { mut v: i64, mut left: Option[T], mut right: Option[T] }

fn main() {
    let head = T { v: 0, left: None, right: None };
    let mut cur = head;
    for i in 1..200000 {
        let c = T { v: i, left: None, right: None };
        cur.left = Some(c);
        cur = c;
    }
    println(f"built {head.v}");
}
"#;

/// Drop bodies over a tree with a subtree another handle keeps, and a
/// deep three-child tree with no `Drop` body.
const ORDER: &str = r#"shared struct N { name: String, mut left: Option[N], mut right: Option[N] }

impl Drop for N {
    fn drop(mut ref self) {
        println(f"drop {self.name}");
    }
}

shared struct P { v: i64, mut a: Option[P], mut b: Option[P], mut c: Option[P] }

fn leaf(s: own String) -> N { N { name: s, left: None, right: None } }

fn main() {
    let kept = leaf("kept");
    {
        let root = N { name: "root", left: None, right: None };
        let l = N { name: "l", left: Some(leaf("ll")), right: Some(kept) };
        let r = N { name: "r", left: Some(leaf("rl")), right: Some(N { name: "rr", left: None, right: Some(leaf("rrr")) }) };
        root.left = Some(l);
        root.right = Some(r);
        println("dropping root");
    }
    println(f"still {kept.name}");
    let top = P { v: 0, a: None, b: None, c: None };
    let mut cur = top;
    for i in 1..30000 {
        let n = P { v: i, a: Some(P { v: -i, a: None, b: None, c: None }), b: None, c: Some(P { v: i * 2, a: None, b: None, c: None }) };
        cur.b = Some(n);
        cur = n;
    }
    println(f"built {top.v}");
}
"#;

const ORDER_OUT: &str =
    "dropping root\ndrop root\ndrop r\ndrop rr\ndrop rrr\ndrop rl\ndrop l\ndrop ll\n\
                         still kept\nbuilt 0\ndrop kept\n";

fn scratch(tag: &str) -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("karac-wasm-deep-drop-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn karac(args: &[&str], file: &Path) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .arg(file)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// `src` built for wasm and run under node: stdout, or `None` when skipped.
fn on_wasm(tag: &str, src: &str) -> Option<String> {
    let archive =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("target/release/libkarac_runtime_wasm.a");
    let node = Command::new("node").arg("--version").output();
    if !archive.exists() || !node.is_ok_and(|o| o.status.success()) {
        eprintln!("skipping mir_wasm_deep_drop::{tag}: no wasm archive or node");
        return None;
    }
    let dir = scratch(tag);
    let file = dir.join("main.kara");
    std::fs::write(&file, src).unwrap();
    std::fs::write(dir.join("run.mjs"), RUNNER).unwrap();
    let wasm = dir.join("main.wasm");
    let built = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(["build", "--target=wasm_wasi"])
        .arg(&file)
        .arg("-o")
        .arg(&wasm)
        .output()
        .unwrap();
    assert!(
        built.status.success(),
        "{}",
        String::from_utf8_lossy(&built.stderr)
    );
    let run = Command::new("node")
        .arg("--no-warnings")
        .arg("--no-turbo-fast-api-calls")
        .arg(dir.join("run.mjs"))
        .arg(&wasm)
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        run.status.success(),
        "{tag}: {}",
        String::from_utf8_lossy(&run.stderr)
    );
    Some(String::from_utf8_lossy(&run.stdout).into_owned())
}

#[test]
fn wasm_drops_a_deep_two_child_chain() {
    if let Some(out) = on_wasm("deep", DEEP) {
        assert_eq!(out, "built 0\n");
    }
}

#[test]
fn tree_release_keeps_the_drop_order_on_every_lane() {
    let dir = scratch("order");
    let file = dir.join("main.kara");
    std::fs::write(&file, ORDER).unwrap();
    assert_eq!(karac(&["__mir-run"], &file), ORDER_OUT, "interp");
    assert_eq!(karac(&["__mir-llvm-run"], &file), ORDER_OUT, "llvm");
    let _ = std::fs::remove_dir_all(&dir);
    if let Some(out) = on_wasm("order", ORDER) {
        assert_eq!(out, ORDER_OUT, "wasm");
    }
}

/// B-2026-11-17-117, the remainder of -311: a node that also holds a
/// `weak` handle or a `String` released recursively. A doubly linked list
/// and a tree with `weak` back links, each 200000 deep, and a tree whose
/// `Drop` bodies read the `weak` parent.
const WEAK: &str = r#"shared struct N { name: String, mut parent: weak N, mut left: Option[N], mut right: Option[N] }

impl Drop for N {
    fn drop(mut ref self) {
        let p = match self.parent.upgrade() { Some(p) => p.name.clone(), None => "-".to_string() };
        println(f"drop {self.name} parent {p}");
    }
}

shared struct L { v: i64, tag: String, mut prev: weak L, mut next: Option[L] }

shared struct T { v: i64, mut up: weak T, mut left: Option[T], mut right: Option[T] }

fn node(s: own String) -> N { N { name: s, parent: None, left: None, right: None } }

fn main() {
    {
        let root = node("root");
        let l = node("l");
        let r = node("r");
        let rl = node("rl");
        l.parent = root;
        r.parent = root;
        rl.parent = r;
        r.left = Some(rl);
        root.left = Some(l);
        root.right = Some(r);
        println("dropping root");
    }
    let head = L { v: 0, tag: "h", prev: None, next: None };
    let mut cur = head;
    for i in 1..200000 {
        let n = L { v: i, tag: f"n{i}", prev: None, next: None };
        n.prev = cur;
        cur.next = Some(n);
        cur = n;
    }
    println(f"list {head.v}");
    let top = T { v: 0, up: None, left: None, right: None };
    let mut c = top;
    for i in 1..200000 {
        let n = T { v: i, up: None, left: None, right: None };
        n.up = c;
        c.left = Some(n);
        c = n;
    }
    println(f"tree {top.v}");
}
"#;

const WEAK_OUT: &str = "dropping root\ndrop root parent -\ndrop r parent -\ndrop rl parent -\ndrop l parent -\nlist 0\ntree 0\n";

#[test]
fn deep_chains_with_weak_back_links_drop_in_order() {
    let dir = scratch("weak");
    let file = dir.join("main.kara");
    std::fs::write(&file, WEAK).unwrap();
    assert_eq!(karac(&["__mir-run"], &file), WEAK_OUT, "interp");
    assert_eq!(karac(&["__mir-llvm-run"], &file), WEAK_OUT, "llvm");
    let _ = std::fs::remove_dir_all(&dir);
    if let Some(out) = on_wasm("weak", WEAK) {
        assert_eq!(out, WEAK_OUT, "wasm");
    }
}

/// B-2026-11-17-138, the next remainder: a node whose handles sit in a
/// `Vec` (an n-ary tree), a `Map` (a trie) or a plain struct (a `Vec` of
/// pairs holding an `Option` handle) still released recursively. Drop bodies
/// over each shape, then each shape 200000 deep. Every map node holds one
/// key, so the order does not depend on the hash seed.
const CONTAINERS: &str = r#"shared struct N { name: String, mut kids: Vec[N] }

impl Drop for N {
    fn drop(mut ref self) {
        println(f"drop N {self.name}");
    }
}

shared struct T { name: String, mut kids: Map[i64, T] }

impl Drop for T {
    fn drop(mut ref self) {
        println(f"drop T {self.name}");
    }
}

struct Pair { a: Option[S], w: i64, b: Option[S] }

shared struct S { name: String, mut items: Vec[Pair], mut one: Pair }

impl Drop for S {
    fn drop(mut ref self) {
        println(f"drop S {self.name}");
    }
}

fn n(s: own String) -> N { N { name: s, kids: Vec.new() } }
fn t(s: own String) -> T { T { name: s, kids: Map.new() } }
fn s(x: own String) -> S { S { name: x, items: Vec.new(), one: Pair { a: None, w: 0, b: None } } }

shared struct Deep { v: i64, mut kids: Vec[Deep] }
shared struct Trie { end: bool, mut kids: Map[i64, Trie] }
struct Hop { a: Option[Link], w: i64 }
shared struct Link { v: i64, mut items: Vec[Hop] }

fn main() {
    let kept = n("kept");
    {
        let root = n("root");
        let a = n("a");
        a.kids.push(n("a0"));
        a.kids.push(kept);
        a.kids.push(n("a2"));
        root.kids.push(a);
        root.kids.push(n("b"));
        let c = n("c");
        c.kids.push(n("c0"));
        root.kids.push(c);
        println("dropping N root");
    }
    println(f"kept {kept.name}");
    {
        let root = t("root");
        let x = t("x");
        x.kids[1] = t("x1");
        root.kids[7] = x;
        println("dropping T root");
    }
    {
        let root = s("root");
        let p = s("p");
        p.one = Pair { a: Some(s("p.one.a")), w: 1, b: Some(s("p.one.b")) };
        root.items.push(Pair { a: Some(p), w: 2, b: None });
        root.items.push(Pair { a: None, w: 3, b: Some(s("q")) });
        root.one = Pair { a: Some(s("r")), w: 4, b: None };
        println("dropping S root");
    }
    println("done");
    let top = Deep { v: 0, kids: Vec.new() };
    let mut cur = top;
    for i in 1..200000 {
        let k = Deep { v: i, kids: Vec.new() };
        cur.kids.push(Deep { v: -i, kids: Vec.new() });
        cur.kids.push(k);
        cur = k;
    }
    println(f"nary {top.v}");
    let trie = Trie { end: false, kids: Map.new() };
    let mut c = trie;
    for i in 1..200000 {
        let k = Trie { end: i == 199999, kids: Map.new() };
        c.kids[i % 26] = k;
        c = k;
    }
    println(f"trie {trie.end}");
    let s0 = Link { v: 0, items: Vec.new() };
    let mut sc = s0;
    for i in 1..200000 {
        let k = Link { v: i, items: Vec.new() };
        sc.items.push(Hop { a: Some(k), w: i });
        sc = k;
    }
    println(f"struct {s0.v}");
}
"#;

const CONTAINERS_OUT: &str = "dropping N root\n\
drop N root\n\
drop N a\n\
drop N a0\n\
drop N a2\n\
drop N b\n\
drop N c\n\
drop N c0\n\
kept kept\n\
dropping T root\n\
drop T root\n\
drop T x\n\
drop T x1\n\
dropping S root\n\
drop S root\n\
drop S r\n\
drop S p\n\
drop S p.one.b\n\
drop S p.one.a\n\
drop S q\n\
done\n\
nary 0\n\
trie false\n\
struct 0\n\
drop N kept\n\
";

#[test]
fn handles_in_vec_map_and_struct_fields_drain_in_order() {
    let dir = scratch("containers");
    let file = dir.join("main.kara");
    std::fs::write(&file, CONTAINERS).unwrap();
    assert_eq!(karac(&["__mir-run"], &file), CONTAINERS_OUT, "interp");
    assert_eq!(karac(&["__mir-llvm-run"], &file), CONTAINERS_OUT, "llvm");
    let _ = std::fs::remove_dir_all(&dir);
    if let Some(out) = on_wasm("containers", CONTAINERS) {
        assert_eq!(out, CONTAINERS_OUT, "wasm");
    }
}

/// B-2026-11-17-139, the remainder of -138: handles inside a plain enum
/// (`Vec[Kid]` with `Kid.Node(E)`, an AST's shape) or inside structs with a
/// `Drop` body (`Vec[Loud]`, a `Map` of them, one nested in a plain struct)
/// still released recursively. Drop bodies over each shape, then an enum
/// tree and a `Vec` of `Drop`-impl holders, each 200000 deep. Every map
/// holds one key, so the order does not depend on the hash seed.
const TASKS: &str = r#"enum Kid { Node(E), Num(i64), Pair(E, String) }

shared struct E { name: String, mut kids: Vec[Kid] }

impl Drop for E {
    fn drop(mut ref self) {
        println(f"drop E {self.name}");
    }
}

struct Loud { tag: String, a: Option[D], b: Option[D] }

impl Drop for Loud {
    fn drop(mut ref self) {
        println(f"drop Loud {self.tag}");
    }
}

shared struct D { name: String, mut items: Vec[Loud], mut solo: Option[Loud] }

impl Drop for D {
    fn drop(mut ref self) {
        println(f"drop D {self.name}");
    }
}

struct Quiet2 { inner: Loud, n: i64 }

shared struct M { name: String, mut by: Map[i64, Loud2], mut q: Vec[Quiet2] }

struct Loud2 { tag: String, m: Option[M] }

impl Drop for Loud2 {
    fn drop(mut ref self) {
        println(f"drop Loud2 {self.tag}");
    }
}

impl Drop for M {
    fn drop(mut ref self) {
        println(f"drop M {self.name}");
    }
}


enum Kid2 { Node(Deep), Num(i64) }
shared struct Deep { v: i64, mut kids: Vec[Kid2] }
struct Hold { a: Option[Chain], w: i64 }
impl Drop for Hold { fn drop(mut ref self) { self.w = 0; } }
shared struct Chain { v: i64, mut items: Vec[Hold] }

fn e(s: own String) -> E { E { name: s, kids: Vec.new() } }
fn d(s: own String) -> D { D { name: s, items: Vec.new(), solo: None } }
fn m(s: own String) -> M { M { name: s, by: Map.new(), q: Vec.new() } }

fn main() {
    let kept = e("kept");
    {
        let root = e("root");
        let a = e("a");
        a.kids.push(Kid.Node(e("a0")));
        a.kids.push(Kid.Num(7));
        a.kids.push(Kid.Pair(kept, "p"));
        a.kids.push(Kid.Node(e("a3")));
        root.kids.push(Kid.Node(a));
        root.kids.push(Kid.Num(1));
        root.kids.push(Kid.Pair(e("b"), "q"));
        println("dropping E root");
    }
    println(f"kept {kept.name}");
    {
        let root = d("root");
        let x = d("x");
        x.items.push(Loud { tag: "x0", a: Some(d("x0a")), b: None });
        x.solo = Some(Loud { tag: "xs", a: None, b: Some(d("xsb")) });
        root.items.push(Loud { tag: "r0", a: Some(x), b: Some(d("r0b")) });
        root.items.push(Loud { tag: "r1", a: None, b: None });
        root.solo = Some(Loud { tag: "rs", a: Some(d("rsa")), b: None });
        println("dropping D root");
    }
    {
        let root = m("root");
        let y = m("y");
        y.q.push(Quiet2 { inner: Loud { tag: "yq", a: Some(d("yqa")), b: None }, n: 1 });
        root.by[5] = Loud2 { tag: "r5", m: Some(y) };
        root.q.push(Quiet2 { inner: Loud { tag: "rq", a: None, b: None }, n: 2 });
        println("dropping M root");
    }
    println("done");
    let e0 = Deep { v: 0, kids: Vec.new() };
    let mut c = e0;
    for i in 1..200000 {
        let k = Deep { v: i, kids: Vec.new() };
        c.kids.push(Kid2.Num(i));
        c.kids.push(Kid2.Node(k));
        c = k;
    }
    println(f"enum {e0.v}");
    let d0 = Chain { v: 0, items: Vec.new() };
    let mut dc = d0;
    for i in 1..200000 {
        let k = Chain { v: i, items: Vec.new() };
        dc.items.push(Hold { a: Some(k), w: i });
        dc = k;
    }
    println(f"dropimpl {d0.v}");
}
"#;

const TASKS_OUT: &str = "dropping E root\n\
drop E root\n\
drop E a\n\
drop E a0\n\
drop E a3\n\
drop E b\n\
kept kept\n\
dropping D root\n\
drop D root\n\
drop Loud rs\n\
drop D rsa\n\
drop Loud r0\n\
drop D r0b\n\
drop D x\n\
drop Loud xs\n\
drop D xsb\n\
drop Loud x0\n\
drop D x0a\n\
drop Loud r1\n\
dropping M root\n\
drop M root\n\
drop Loud rq\n\
drop Loud2 r5\n\
drop M y\n\
drop Loud yq\n\
drop D yqa\n\
done\n\
enum 0\n\
dropimpl 0\n\
drop E kept\n\
";

#[test]
fn enum_and_drop_impl_elements_drain_in_order() {
    let dir = scratch("tasks");
    let file = dir.join("main.kara");
    std::fs::write(&file, TASKS).unwrap();
    assert_eq!(karac(&["__mir-run"], &file), TASKS_OUT, "interp");
    assert_eq!(karac(&["__mir-llvm-run"], &file), TASKS_OUT, "llvm");
    let _ = std::fs::remove_dir_all(&dir);
    if let Some(out) = on_wasm("tasks", TASKS) {
        assert_eq!(out, TASKS_OUT, "wasm");
    }
}
