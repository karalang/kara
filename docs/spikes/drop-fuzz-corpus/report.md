# drop_fuzz report

- programs generated: **400**
- programs valid (compiled+ran on ≥1 surface): **400**
- valid (program,surface) executions: **1200**
- base seed: `5000`
- elapsed: 288.0s

## Ownership-oracle self-check (Slice 3)

The executable judgment ran on **400** generated programs, scheduling **5793** drops, with **0** invariant violation(s). The generator emits only ownership-clean programs, so a nonzero violation count means the model and the generator disagree (an oracle bug or a checker gap).

## Measured drop-bug rate

**523** memory-safety findings over **1200** valid executions = **43.58%**.

## Signatures (bucketed corpus)

| signature | count | memory-safety |
|---|---|---|
| `autopar:drop-never-ran` | 196 | yes |
| `interp:drop-never-ran` | 131 | yes |
| `seq:drop-never-ran` | 196 | yes |

## Minimal repros

### repro 1 — `seq:drop-never-ran` (seed 5002)

<details><summary>sanitizer excerpt</summary>

```
user `impl Drop` body count != construction count
  tag 1: constructed 40x, dropped 0x
  tag 2: constructed 40x, dropped 0x

```

</details>

```rust
struct Payload { tag: i64, name: String, items: Vec[String] }

shared struct Holder { s: String }

shared enum Tree { Leaf(String), Node(Tree) }

fn take_str(s: String) -> i64 { return s.len(); }

fn echo_vec(v: Vec[String]) -> Vec[String] { return v; }

fn hold_len(h: Holder) -> i64 { return h.s.len(); }

fn peek(s: ref String) -> i64 { return s.len(); }

fn grow(v: mut ref Vec[String]) {
}

fn band(data: Vec[String], lo: i64) -> i64 {
    let mut acc: i64 = 0i64;
    return acc;
}

fn tree_len(t: Tree) -> i64 {
    match t {
        Leaf(s) => s.len(),
        Node(inner) => tree_len(inner),
    }
}

struct Tracked { tag: i64, name: String }

impl Drop for Tracked {
    fn drop(mut ref self) { println(f"D{self.tag}"); }
}

fn new_tracked(tag: i64, name: String) -> Tracked {
    println(f"N{tag}");
    return Tracked { tag: tag, name: name };
}

fn tracked_len(t: Tracked) -> i64 { return t.name.len(); }

fn tracked_peek(t: ref Tracked) -> i64 { return t.name.len(); }

struct Crate { lid: i64, item: Tracked }

fn new_crate(tag: i64, name: String) -> Crate {
    return Crate { lid: tag, item: new_tracked(tag, name) };
}

fn tup_tracked_len(t: (i64, Tracked)) -> i64 { return t.0 + t.1.name.len(); }

fn tup_vec_len(t: (Vec[String], i64)) -> i64 {
    let mut acc: i64 = t.1;
    return acc;
}

fn peek_vec(v: ref Vec[String]) -> i64 { return v.len(); }

fn arr_inner_peek(a: ref Array[Tracked, 2]) -> i64 { return a[1].name.len(); }

enum Slot[T] { Filled(T), Blank }

struct Wrap[T] { item: T }


enum Bin { Packed(Array[Tracked, 2]), Bare }

fn slot_str_peek(s: ref Slot[String]) -> i64 {
    match s {
        Filled(x) => x.len(),
        Blank => 0i64,
    }
}

fn slot_tracked_peek(s: ref Slot[Tracked]) -> i64 {
    match s {
        Filled(t) => t.name.len(),
        Blank => 0i64,
    }
}

fn bin_peek(b: ref Bin) -> i64 {
    match b {
        Packed(a) => arr_inner_peek(a),
        Bare => 0i64,
    }
}
fn main() {
    let mut acc: i64 = 0i64;
    let mut round: i64 = 0i64;
    while round < 40i64 {
        let mut bn3: Bin = Packed([new_tracked(1i64, "df12_payload_bytes_kept_long_enough_for_lsan_12".to_string()), new_tracked(2i64, "df13_payload_bytes_kept_long_enough_for_lsan_13".to_string())]);
        round = round + 1i64;
    }
    println(acc);
}
```

### repro 2 — `autopar:drop-never-ran` (seed 5002)

<details><summary>sanitizer excerpt</summary>

```
user `impl Drop` body count != construction count
  tag 1: constructed 40x, dropped 0x
  tag 2: constructed 40x, dropped 0x

```

</details>

```rust
struct Payload { tag: i64, name: String, items: Vec[String] }

shared struct Holder { s: String }

shared enum Tree { Leaf(String), Node(Tree) }

fn take_str(s: String) -> i64 { return s.len(); }

fn echo_vec(v: Vec[String]) -> Vec[String] { return v; }

fn hold_len(h: Holder) -> i64 { return h.s.len(); }

fn peek(s: ref String) -> i64 { return s.len(); }

fn grow(v: mut ref Vec[String]) {
}

fn band(data: Vec[String], lo: i64) -> i64 {
    let mut acc: i64 = 0i64;
    return acc;
}

fn tree_len(t: Tree) -> i64 {
    match t {
        Leaf(s) => s.len(),
        Node(inner) => tree_len(inner),
    }
}

struct Tracked { tag: i64, name: String }

impl Drop for Tracked {
    fn drop(mut ref self) { println(f"D{self.tag}"); }
}

fn new_tracked(tag: i64, name: String) -> Tracked {
    println(f"N{tag}");
    return Tracked { tag: tag, name: name };
}

fn tracked_len(t: Tracked) -> i64 { return t.name.len(); }

fn tracked_peek(t: ref Tracked) -> i64 { return t.name.len(); }

struct Crate { lid: i64, item: Tracked }

fn new_crate(tag: i64, name: String) -> Crate {
    return Crate { lid: tag, item: new_tracked(tag, name) };
}

fn tup_tracked_len(t: (i64, Tracked)) -> i64 { return t.0 + t.1.name.len(); }

fn tup_vec_len(t: (Vec[String], i64)) -> i64 {
    let mut acc: i64 = t.1;
    return acc;
}

fn peek_vec(v: ref Vec[String]) -> i64 { return v.len(); }

fn arr_inner_peek(a: ref Array[Tracked, 2]) -> i64 { return a[1].name.len(); }

enum Slot[T] { Filled(T), Blank }

struct Wrap[T] { item: T }


enum Bin { Packed(Array[Tracked, 2]), Bare }

fn slot_str_peek(s: ref Slot[String]) -> i64 {
    match s {
        Filled(x) => x.len(),
        Blank => 0i64,
    }
}

fn slot_tracked_peek(s: ref Slot[Tracked]) -> i64 {
    match s {
        Filled(t) => t.name.len(),
        Blank => 0i64,
    }
}

fn bin_peek(b: ref Bin) -> i64 {
    match b {
        Packed(a) => arr_inner_peek(a),
        Bare => 0i64,
    }
}
fn main() {
    let mut acc: i64 = 0i64;
    let mut round: i64 = 0i64;
    while round < 40i64 {
        let mut bn3: Bin = Packed([new_tracked(1i64, "df12_payload_bytes_kept_long_enough_for_lsan_12".to_string()), new_tracked(2i64, "df13_payload_bytes_kept_long_enough_for_lsan_13".to_string())]);
        round = round + 1i64;
    }
    println(acc);
}
```

### repro 3 — `interp:drop-never-ran` (seed 5002)

<details><summary>sanitizer excerpt</summary>

```
user `impl Drop` body count != construction count
  tag 1: constructed 40x, dropped 0x
  tag 2: constructed 40x, dropped 0x

```

</details>

```rust
struct Payload { tag: i64, name: String, items: Vec[String] }

shared struct Holder { s: String }

shared enum Tree { Leaf(String), Node(Tree) }

fn take_str(s: String) -> i64 { return s.len(); }

fn echo_vec(v: Vec[String]) -> Vec[String] { return v; }

fn hold_len(h: Holder) -> i64 { return h.s.len(); }

fn peek(s: ref String) -> i64 { return s.len(); }

fn grow(v: mut ref Vec[String]) {
}

fn band(data: Vec[String], lo: i64) -> i64 {
    let mut acc: i64 = 0i64;
    return acc;
}

fn tree_len(t: Tree) -> i64 {
    match t {
        Leaf(s) => s.len(),
        Node(inner) => tree_len(inner),
    }
}

struct Tracked { tag: i64, name: String }

impl Drop for Tracked {
    fn drop(mut ref self) { println(f"D{self.tag}"); }
}

fn new_tracked(tag: i64, name: String) -> Tracked {
    println(f"N{tag}");
    return Tracked { tag: tag, name: name };
}

fn tracked_len(t: Tracked) -> i64 { return t.name.len(); }

fn tracked_peek(t: ref Tracked) -> i64 { return t.name.len(); }

struct Crate { lid: i64, item: Tracked }

fn new_crate(tag: i64, name: String) -> Crate {
    return Crate { lid: tag, item: new_tracked(tag, name) };
}

fn tup_tracked_len(t: (i64, Tracked)) -> i64 { return t.0 + t.1.name.len(); }

fn tup_vec_len(t: (Vec[String], i64)) -> i64 {
    let mut acc: i64 = t.1;
    return acc;
}

fn peek_vec(v: ref Vec[String]) -> i64 { return v.len(); }

fn arr_inner_peek(a: ref Array[Tracked, 2]) -> i64 { return a[1].name.len(); }

enum Slot[T] { Filled(T), Blank }

struct Wrap[T] { item: T }


enum Bin { Packed(Array[Tracked, 2]), Bare }

fn slot_str_peek(s: ref Slot[String]) -> i64 {
    match s {
        Filled(x) => x.len(),
        Blank => 0i64,
    }
}

fn slot_tracked_peek(s: ref Slot[Tracked]) -> i64 {
    match s {
        Filled(t) => t.name.len(),
        Blank => 0i64,
    }
}

fn bin_peek(b: ref Bin) -> i64 {
    match b {
        Packed(a) => arr_inner_peek(a),
        Bare => 0i64,
    }
}
fn main() {
    let mut acc: i64 = 0i64;
    let mut round: i64 = 0i64;
    while round < 40i64 {
        let mut bn3: Bin = Packed([new_tracked(1i64, "df12_payload_bytes_kept_long_enough_for_lsan_12".to_string()), new_tracked(2i64, "df13_payload_bytes_kept_long_enough_for_lsan_13".to_string())]);
        round = round + 1i64;
    }
    println(acc);
}
```

