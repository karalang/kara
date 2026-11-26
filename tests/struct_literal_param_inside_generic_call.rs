//! B-2026-11-17-133: a generic struct's parameter left open inside a generic
//! call in one of its fields (`Holder { values: Mutex.new(VecDeque.new()) }`,
//! `Bag { items: Some(Vec.new()) }`) is fixed by a later use, as it is when
//! the field holds the value directly (`Holder { values: VecDeque.new() }`,
//! B-2026-10-19-9). The struct's own `T` came back from the field slot into
//! the inner `VecDeque.new()` / `Vec.new()`, and only the outer value took
//! the inference variable, so MIR lowering met an unknown `T`: E0516 "an
//! expression whose type does not lower (Type(UnknownParam(\"T\")))".
//! Measured with the fix's call disabled: both programs below stop there.
//! The compiled backends run the same program as the corpus pin
//! `corpus/design/ok_struct_literal_param_open_inside_a_generic_call`.

use std::process::{Command, Output};

fn karac(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .output()
        .expect("karac")
}

fn text(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

const SRC: &str = r#"import std.sync.{Mutex};
struct Holder[T] {
    values: Mutex[VecDeque[T]],
}

impl[T] Holder[T] {
    fn put(self, e: own T) {
        self.values.lock().value.push_back(e);
    }

    fn size(self) -> i64 {
        self.values.lock().value.len()
    }
}

struct Bag[T] {
    items: Option[Vec[T]],
}

impl[T] Bag[T] {
    fn add(mut ref self, x: own T) {
        if let Some(mut ref v) = self.items {
            v.push(x);
        }
    }

    fn count(self) -> i64 {
        match self.items {
            Some(ref v) => v.len(),
            None => 0,
        }
    }
}

fn main() {
    let h = Holder { values: Mutex.new(VecDeque.new()) };
    h.put(5);
    h.put(6);
    println(h.size());
    let mut b = Bag { items: Some(Vec.new()) };
    b.add("x".to_string());
    b.add("y".to_string());
    b.add("z".to_string());
    println(b.count());
}
"#;

const WANT: &str = "2\n3\n";

fn write_src(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("karac-open-param-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("main.kara");
    std::fs::write(&path, SRC).unwrap();
    path
}

#[test]
fn a_param_open_inside_a_generic_call_in_a_field_is_fixed_by_a_later_use() {
    let path = write_src("interp");
    let p = path.to_str().unwrap();
    let check = karac(&["check", p]);
    let run = karac(&["run", "--interp", p]);
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
    assert!(check.status.success(), "{}", text(&check));
    assert!(run.status.success(), "{}", text(&run));
    assert_eq!(String::from_utf8_lossy(&run.stdout), WANT);
}
