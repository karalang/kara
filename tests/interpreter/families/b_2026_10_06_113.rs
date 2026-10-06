//! B-2026-10-06-113: an enum's hand-written `cmp` is called by `.cmp()` and
//! by the ordered operators under the interpreter.

use super::*;

/// B-2026-10-06-113: `Size` orders `Large` before `Small`, the reverse of
/// declaration order. `.cmp()`, `<`, `>`, `<=` and a generic `T: Ord` caller
/// all follow the impl; a derive-only enum keeps declaration order.
#[test]
fn test_enum_hand_written_cmp_is_called() {
    let out = run(r#"#[derive(PartialEq, Eq, Clone, Copy)]
enum Size { Small, Large }
impl PartialOrd for Size {
    fn partial_cmp(ref self, other: ref Size) -> Option[Ordering] { Some(self.cmp(other)) }
}
impl Ord for Size {
    fn cmp(ref self, other: ref Size) -> Ordering {
        match (self, other) {
            (Size.Small, Size.Large) => Ordering.Greater,
            (Size.Large, Size.Small) => Ordering.Less,
            _ => Ordering.Equal,
        }
    }
}
#[derive(PartialEq, Eq, PartialOrd, Ord, Clone, Copy)]
enum Plain { A, B }
fn smaller[T: Ord](a: T, b: T) -> bool { a.cmp(b) == Ordering.Less }
fn main() {
    println(f"{Size.Small.cmp(Size.Large) == Ordering.Greater}");
    println(f"{Size.Small < Size.Large} {Size.Small > Size.Large} {Size.Large <= Size.Small}");
    println(f"{smaller(Size.Large, Size.Small)}");
    println(f"{Plain.A < Plain.B} {Plain.A.cmp(Plain.B) == Ordering.Less}");
}
"#);
    assert_eq!(out, "true\nfalse true true\ntrue\ntrue true\n");
}
