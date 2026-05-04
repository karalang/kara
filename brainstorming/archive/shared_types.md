# Shared Types: Reference-Semantics for Trees & Graphs

**Status:** Canonical semantics live in design.md. This document is kept as a detailed reference for design rationale, implementation considerations, and alternatives explored.

**Problem:** Kāra's ownership model (move-by-default, `ref`/`mut ref` borrows, invisible RC fallback) handles most data structures naturally. But trees, graphs, and linked lists — where multiple references to the same node are the norm — require `Arena<T>` + `ArenaRef<T>`, which feels like a workaround rather than first-class support.

**Goal:** Make shared/recursive data structures feel as natural as structs and enums, without compromising the ownership model for everything else.

---

## Proposal: `shared struct`

A single keyword that opts a struct into reference semantics with automatic reference counting:

```
shared struct TreeNode {
    val: i64,
    left: Option<TreeNode>,
    right: Option<TreeNode>,
}
```

### Semantics

| Behavior | Regular `struct` | `shared struct` |
|---|---|---|
| Assignment | Move | RC increment (shallow copy of reference) |
| Equality | Value comparison | No default `==`; require `#[derive(Eq)]` for structural, `ref_eq()` for identity |
| Mutation | Owned or `mut ref` | Interior mutability (like Swift `class`) |
| Destruction | Deterministic drop | RC decrement, drop at zero |
| Cycles | Compile-time rejected | `weak` required on back-edges |

### Basic Usage

```
shared struct ListNode {
    val: i64,
    next: Option<ListNode>,
}

// Construction — looks identical to regular structs
let a = ListNode { val: 1, next: None };
let b = ListNode { val: 2, next: Some(a) };

// Assignment shares, not moves
let c = b;       // c and b point to the same node
b.val = 99;      // c.val is also 99 — shared reference
```

### Trees

```
shared struct TreeNode {
    val: i64,
    left: Option<TreeNode>,
    right: Option<TreeNode>,
}

fn insert(node: Option<TreeNode>, val: i64) -> TreeNode {
    match node {
        None => TreeNode { val: val, left: None, right: None },
        Some(n) => {
            if val < n.val {
                n.left = Some(insert(n.left, val));
            } else {
                n.right = Some(insert(n.right, val));
            }
            n
        }
    }
}
```

### Graphs with Cycles

```
shared struct GraphNode {
    id: u64,
    neighbors: Vec<weak GraphNode>,   // weak prevents RC cycles
}

fn connect(a: GraphNode, b: GraphNode) {
    a.neighbors.push(b);   // implicit strong → weak conversion
    b.neighbors.push(a);   // bidirectional, no cycle leak
}

// Accessing a weak reference returns Option
fn first_neighbor_id(node: GraphNode) -> Option<u64> {
    match node.neighbors.first() {
        Some(weak_ref) => {
            match weak_ref.upgrade() {
                Some(neighbor) => Some(neighbor.id),
                None => None,   // neighbor was deallocated
            }
        }
        None => None,
    }
}
```

### Linked List — Reverse

```
shared struct ListNode {
    val: i64,
    next: Option<ListNode>,
}

fn reverse(head: Option<ListNode>) -> Option<ListNode> {
    let mut prev = None;
    let mut curr = head;
    while let Some(node) = curr {
        let next = node.next;
        node.next = prev;
        prev = Some(node);
        curr = next;
    }
    prev
}
```

---

## Resolved Questions

### 1. Interior Mutability — RESOLVED: Mutable by default

`shared struct` fields are mutable by default (like Swift `class`). No `let mut` required:

```
let node = TreeNode { val: 1, left: None, right: None };
node.val = 2;   // just works — shared implies mutable
```

**Rationale:** The whole point of `shared struct` is ergonomic tree/graph manipulation. Requiring `mut` on every node defeats the purpose. Data race safety comes from the effect system and concurrency runtime, not mutability restrictions.

### 2. Rc vs Arc — RESOLVED: Rc default, Arc when compiler detects cross-task usage

Consistent with existing invisible RC design. The effect system already knows when values cross concurrency boundaries — single-threaded code gets faster Rc, multi-threaded code automatically gets Arc. User never thinks about it.

**Arc synchronization (brainstorming v9 §1):** When the compiler promotes to `Arc`, `shared struct` is `Send` but not `Sync`. Concurrent `mut` field access from multiple tasks requires explicit `Mutex[T]` wrapping with `lock` block syntax:

```kara
let node = Mutex(TreeNode { val: 1, left: None, right: None })
lock node { node.val = 42 }
```

`RefCell` semantics remain sufficient under `Arc` because the compiler rejects concurrent mutation without `Mutex`.

### 3. Interaction with Traits — RESOLVED: `self` is always a shared reference (RC increment)

`self` in a `shared struct` method is a shared reference. The method borrows the value for the duration of the call (RC increment on entry, decrement on return). The node is NOT consumed:

```
impl TreeNode {
    fn depth(self) -> i64 {
        // self is a shared reference — node is not consumed
        match (self.left, self.right) {
            (None, None) => 1,
            (Some(l), Some(r)) => 1 + max(l.depth(), r.depth()),
            _ => ...,
        }
    }
}

let root = build_tree();
let d = root.depth();    // root is still valid after this call
```

Matches Swift's `class` method semantics.

### 4. Comparison Semantics — RESOLVED: No default `==`, require `#[derive(Eq)]`

`shared struct` has no built-in equality. Use `#[derive(Eq)]` for structural equality or implement `Eq` manually. Reference identity available via `ref_eq()` function.

```
#[derive(Eq)]
shared struct TreeNode { val: i64, left: Option<TreeNode>, right: Option<TreeNode> }

let a = TreeNode { val: 1, left: None, right: None };
let b = a;
a == b         // true — structural equality (same val, same children)
ref_eq(a, b)   // true — same reference (a and b point to same node)
```

**Rationale:** Consistent with G27 (derive is explicit opt-in). Not every shared struct should be equatable.

### 5. Pattern Matching — RESOLVED: Destructuring increments RC

Destructuring a `shared struct` binds new names to the shared fields, each incrementing RC. When bindings go out of scope, RC decrements:

```
match tree {
    TreeNode { val, left: Some(l), right: Some(r) } => {
        // val, l, r are all shared references (RC incremented)
        // RC decremented when this arm's scope ends
    },
    TreeNode { val, left: None, right: None } => ...,
}
```

Same semantics as assignment — destructuring creates new shared references, not copies.

### 6. `shared enum` — RESOLVED: Yes, in scope

`shared` applies to enums too. Recursive enums (JSON, AST) need sharing just like recursive structs:

```
shared enum Json {
    Null,
    Bool { val: bool },
    Number { val: f64 },
    Str { val: String },
    Array { items: Vec<Json> },
    Object { fields: HashMap<String, Json> },
}
```

Same RC semantics as `shared struct` — assignment shares, not moves.

### 7. Arena — RESOLVED: Stdlib Primitive, Not Language Feature

`Arena<T>` is **not** a language feature. It will be a stdlib type added when needed (Phase 8+, or post-Phase 10).

**Rationale:**
- No mainstream language has arena as a language keyword (Go tried it, then removed it in 1.23).
- `shared struct` covers all correctness/ergonomics cases that Arena was needed for.
- Arena as stdlib produces identical performance to a language feature — same machine code after compilation.
- Stdlib types can be added, changed, or replaced without breaking the language.
- A language feature is permanent — stdlib is reversible.

**Arena's future role:** Optional performance escape hatch for cache-friendly bulk allocation (millions of nodes in tight loops). Users who need it import it from stdlib. Most users never touch it.

### 8. Effect System Interaction — RESOLVED: No effect needed for shared mutation

Mutating a `shared struct` is a local in-memory operation — no effect annotation needed. The effect system tracks I/O and resource access (reads/writes to databases, filesystem, network), not in-memory mutation. Data race prevention comes from the concurrency runtime (auto-concurrency only parallelizes non-conflicting effects, so two tasks won't mutate the same shared struct simultaneously unless the programmer explicitly creates that situation).

---

## Alternatives Considered

### First-Class Arena Sugar (`arena struct`)

```
arena struct TreeNode {
    val: i64,
    left: Option<TreeNode>,
    right: Option<TreeNode>,
}
```

Compiler auto-manages the arena. References are implicitly `ArenaRef` under the hood.

**Rejected because:** Arena scope/lifetime management is complex. When does the arena get freed? Scoped arenas need explicit boundaries. The ergonomic gain over `Arena<T>` + `ArenaRef<T>` doesn't justify the implicit lifetime management complexity.

### Do Nothing

Accept that trees/graphs require `Arena` + `ArenaRef`. Kāra is a systems language; systems programmers understand arenas.

**Rejected because:** Trees and graphs are fundamental data structures. A language that makes them awkward will always carry an asterisk. The cost of `shared struct` (one keyword, leveraging existing RC) is low relative to the coverage gained.

### Invisible RC Handles It

The compiler already has invisible RC fallback. Maybe it just needs to be smarter about detecting tree/graph patterns and applying RC automatically.

**Rejected because:** Invisible magic that sometimes works and sometimes doesn't is worse than an explicit opt-in. The programmer should declare intent ("this type is shared") rather than hoping the compiler figures it out.

---

## Implementation Considerations

1. **Parser:** New keyword `shared` before `struct` (and possibly `enum`).
2. **Type checker:** `shared` types use reference semantics — assignment is RC increment, not move.
3. **Ownership checker:** `shared` types are never "moved" in the traditional sense. RC manages lifetime.
4. **Interpreter (Phase 4):** `shared` values are heap-allocated with a reference count. Assignment clones the reference, not the value.
5. **Codegen (Phase 7):** `shared struct` compiles to `Rc<T>` or `Arc<T>` depending on concurrency analysis.
6. **Cycle detection:** Existing type graph cycle detection applies. `weak` still required on back-edges.

---

## Decision Criteria

All resolved. Ready to promote to design.md.

- [x] Interior mutability model — **Mutable by default**
- [x] Rc/Arc selection strategy — **Rc default, Arc when compiler detects cross-task usage**
- [x] `self` semantics in impl blocks — **Shared reference (RC increment), not move**
- [x] Comparison semantics — **No default `==`, require `#[derive(Eq)]`**
- [x] Pattern matching semantics — **Destructure = RC increment**
- [x] Whether `shared enum` is in scope — **Yes**
- [x] Effect system interaction — **No effect needed for in-memory mutation**
- [x] Confirm Arena is stdlib-only — **Yes, stdlib**
