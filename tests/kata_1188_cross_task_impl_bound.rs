//! B-2026-11-17-131: an impl's `CrossTask` bound is computed, as a
//! function's is. `impl[T: CrossTask] BoundedBlockingQueue[T]` refused
//! every call on a `BoundedBlockingQueue[String]` ("`String` does not
//! implement `CrossTask`"), and on `i64` and `Vec[i64]` alike, where
//! `fn f[T: CrossTask](x: T)` took a `String`: the impl gate looked for an
//! impl-table entry, which a computed bound never has.

use std::process::Command;

fn run(tag: &str, sub: &str, src: &str) -> (bool, String) {
    let dir = std::env::temp_dir().join(format!(
        "karac-cross-task-impl-{tag}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.kara");
    std::fs::write(&file, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args([sub, file.to_str().unwrap()])
        .output()
        .expect("karac");
    let _ = std::fs::remove_dir_all(&dir);
    (
        out.status.success(),
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    )
}

const BOXED: &str = "struct Boxed[T: CrossTask] {\n    v: T,\n}\n\n\
    impl[T: CrossTask] Boxed[T] {\n    fn get(self) -> i64 { 1 }\n    \
    fn make(v: own T) -> Boxed[T] { Boxed { v } }\n}\n\n";

#[test]
fn an_impl_cross_task_bound_takes_owned_values() {
    let src = format!(
        "{BOXED}fn main() {{\n    let a: Boxed[String] = Boxed {{ v: \"a\".to_string() }};\n    \
         let b: Boxed[i64] = Boxed {{ v: 3 }};\n    let c: Boxed[Vec[i64]] = Boxed {{ v: [1] }};\n    \
         let d = Boxed.make((1, \"d\".to_string()));\n    \
         println(f\"{{a.get() + b.get() + c.get() + d.get()}}\");\n}}\n"
    );
    let (ok, out) = run("owned", "__mir-run", &src);
    assert!(ok, "{out}");
    assert_eq!(out, "4\n");
}

#[test]
fn an_impl_cross_task_bound_still_refuses_a_shared_type() {
    let src = format!(
        "shared struct Node {{\n    x: i64,\n}}\n\n{BOXED}fn main() {{\n    \
         let b: Boxed[Node] = Boxed {{ v: Node {{ x: 1 }} }};\n    println(f\"{{b.get()}}\");\n}}\n"
    );
    let (ok, out) = run("shared", "check", &src);
    assert!(!ok, "{out}");
    assert!(out.contains("`Node` cannot cross a task boundary"), "{out}");
}

#[test]
fn a_generic_sync_queue_of_strings_runs_across_tasks() {
    let src = "import std.sync.{Semaphore};\nimport std.sync.{Mutex, TaskGroup};\n\n\
        sync struct Queue[T: CrossTask] {\n    slots: Semaphore,\n    items: Semaphore,\n    \
        values: Mutex[VecDeque[T]],\n}\n\n\
        impl[T: CrossTask] Queue[T] {\n    fn new(capacity: i64) -> Queue[T] {\n        \
        Queue { slots: Semaphore.new(capacity), items: Semaphore.new(0), values: Mutex.new(VecDeque.new()) }\n    }\n\n    \
        fn enqueue(self, element: own T) {\n        let _ = self.slots.acquire(60000);\n        \
        self.values.lock().value.push_back(element);\n        self.items.release();\n    }\n\n    \
        fn dequeue(self) -> T {\n        let _ = self.items.acquire(60000);\n        \
        let element = self.values.lock().value.pop_front().unwrap();\n        self.slots.release();\n        element\n    }\n}\n\n\
        fn main() {\n    let q: Queue[String] = Queue.new(2);\n    let out: Mutex[Vec[String]] = Mutex.new([]);\n    {\n        \
        let group = TaskGroup.new();\n        group.spawn(|| {\n            for w in [\"a\", \"b\", \"c\", \"d\"] {\n                \
        q.enqueue(w.to_string());\n            }\n        });\n        group.spawn(|| {\n            for _ in 0..4 {\n                \
        let w = q.dequeue();\n                out.lock().value.push(w);\n            }\n        });\n    }\n    \
        println(f\"{out.lock().value}\");\n}\n";
    let (ok, out) = run("queue", "__mir-run", src);
    assert!(ok, "{out}");
    assert_eq!(out, "[a, b, c, d]\n");
}
