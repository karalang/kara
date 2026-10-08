# Collections

The standard collections are library types written in Kāra, not language primitives.

| Type | Rust equivalent | Purpose |
|---|---|---|
| `Array[T, N]` | `[T; N]` | Fixed-size array |
| `Vec[T]` | `Vec<T>` | Dynamic array |
| `Slice[T]` | `&[T]` / `&mut [T]` | Borrowed view into contiguous memory ([design.md §5](../design.md#5-types)) |
| `Map[K, V]` | `HashMap<K, V>` | Key-value lookup, `K: Hash + Eq` |
| `Set[T]` | `HashSet<T>` | Unique values, `T: Hash + Eq` |
| `SortedMap[K, V]` | `BTreeMap<K, V>` | Key-value lookup in key order, `K: Ord` |
| `SortedSet[T]` | `BTreeSet<T>` | Unique values in ascending order, `T: Ord`; min, max and range queries |
| `VecDeque[T]` | `VecDeque<T>` | Double-ended queue (ring buffer) |
| `PriorityQueue[T]` | `BinaryHeap<T>` | Binary heap, `T: Ord`. Smallest-first by default, the opposite of Rust's `BinaryHeap` |

`String` and `Str` are in [strings.md](strings.md); `Option` and `Result` are in [core-types.md](core-types.md).

**Effects.** A method that grows or reallocates a collection allocates, and a method noted "panics if ..." can panic. Both effects are default-permitted ([README](README.md#signature-conventions)), so the signatures below omit them. A method that takes a function parameter has that argument's effects at each call.

## Collection literals

The literal forms (`[a, b]`, `[v; n]`, `[k: v]`, `[]` and `[:]`), and how an expected type picks the collection a literal builds, are in [design.md §5](../design.md#collection-literals). Where no expected type is available, and for the collections no literal names, the `from` constructors build a collection from a list:

```kara
let primes = Set.from([2, 3, 5, 7]);                 // Set[i64]
let ages = Map.from([("ann", 31), ("bo", 27)]);      // Map[String, i64]
let pq = PriorityQueue.from([9, 7, 8]);              // PriorityQueue[i64]
```

## `Array[T, N]`

A fixed-size array of `N` elements, stored inline. It has every [`Slice[T]`](#slicet) method, `as_slice(self) -> Slice[T]`, and indexing. An `Array[T, N]` argument passes as `Slice[T]`.

**Definite assignment.** `let arr: Array[T, N];` without an initializer reserves the storage and initializes nothing. Reading `arr` before a whole-value assignment is a compile error, as for any binding ([design.md §6](../design.md#6-expressions-and-statements)). Assigning every slot one at a time does not count: the analysis tracks whole-value assignment only. To fill slot by slot, initialize the whole array first (`let mut arr: Array[i64, 8] = [0; 8];`) and overwrite, or build it with `Array.from_fn(n, f)`.

## `Slice[T]`

`Slice[T]` is the borrowed view of a contiguous sequence. Its type and borrow rules are in [design.md §5](../design.md#slices). A `Vec[T]` or `Array[T, N]` receiver finds every method below: method lookup searches the type's own methods first, then `Slice[T]`'s, through the same coercion that passes a `Vec[T]` argument as `Slice[T]` ([design.md §9](../design.md#method-resolution)).

**Reading.**

| Method | Signature | Notes |
|---|---|---|
| `len` | `fn len(self) -> i64` | Number of elements |
| `is_empty` | `fn is_empty(self) -> bool` | `self.len() == 0` |
| `get` | `fn get(self, idx: i64) -> Option[ref T]` | `None` if `idx` is out of bounds. `v[i]` panics instead |
| `first` | `fn first(self) -> Option[ref T]` | `None` if empty |
| `last` | `fn last(self) -> Option[ref T]` | `None` if empty |
| `contains` | `fn contains(self, val: T) -> bool where T: PartialEq` | Linear search |
| `binary_search` | `fn binary_search(self, val: T) -> Option[i64] where T: Ord` | Index of an element equal to `val`, or `None`. The sequence must be sorted ascending |
| `is_sorted` | `fn is_sorted(self) -> bool where T: Ord` | Non-strict ascending check: equal neighbours count as sorted, and 0 or 1 elements are sorted |
| `split_at` | `fn split_at(self, mid: i64) -> (Slice[T], Slice[T])` | `[0, mid)` and `[mid, len)`. Panics if `mid > self.len()` |
| `windows` | `fn windows(self, size: i64) -> Vec[Slice[T]]` | Every run of `size` consecutive elements |
| `chunks` | `fn chunks(self, size: i64) -> Vec[Slice[T]]` | Consecutive runs of `size` elements; the last may be shorter |
| `to_vec` | `fn to_vec(self) -> Vec[T] where T: Clone` | Owned copy |
| `join` | `fn join(self, sep: Str) -> String where T: Display` | Displays each element, separated by `sep` |
| `[]` | `v[i]` and `v[a..b]` | Index operator through `Index` ([design.md §9](../design.md#9-traits)). `v[i]` is `ref T` and panics if out of bounds; `v[a..b]` is a `Slice[T]` |

**Changing in place.** On a `Slice[T]` these need a `mut Slice[T]`.

| Method | Signature | Notes |
|---|---|---|
| `sort` | `fn sort(mut ref self) where T: Ord` | Stable sort, ascending |
| `sort_by` | `fn sort_by(mut ref self, cmp: own MutFn(T, T) -> Ordering)` | Stable sort by a comparison: `w.sort_by(f64.total_cmp)` |
| `sort_by_key` | `fn sort_by_key[K: Ord](mut ref self, key: own MutFn(T) -> K)` | Stable sort by a derived key |
| `reverse` | `fn reverse(mut ref self)` | Reverses in place |
| `fill` | `fn fill(mut ref self, val: T) where T: Clone` | Sets every element to a copy of `val` |
| `swap` | `fn swap(mut ref self, i: i64, j: i64)` | Exchanges the elements at `i` and `j`. Neither value is destroyed, so no `Drop` body runs. Panics if either index is out of bounds; `i == j` does nothing. This is the way to exchange two elements: `let t = xs[i]; xs[i] = xs[j]; xs[j] = t` is rejected for a non-`Copy` `T` |
| `clone_from_slice` | `fn clone_from_slice(mut ref self, src: Slice[T]) where T: Clone` | Overwrites every element with a clone of the matching element of `src`. Panics if the lengths differ. The element-wise copy for a non-`Copy` `T`: `v[a..b].clone_from_slice(other)` |
| `split_at_mut` | `fn split_at_mut(mut ref self, mid: i64) -> (mut Slice[T], mut Slice[T])` | Two disjoint mutable halves, `[0, mid)` and `[mid, len)`. Panics if `mid > self.len()`. Both halves may be live at once because their ranges cannot overlap; the sequence itself may not be used while either is live. Handing one half to each task gives statically disjoint parallel writes |
| `[]=` | `v[i] = x` | Index assignment through `IndexMut` ([design.md §9](../design.md#index-and-indexmut)). Panics if out of bounds |
| `[a..b]=` | `v[a..b] = other` | Copies the elements of `other` into the range. Requires `T: Copy`; panics if the lengths differ. For other `T`, use `clone_from_slice` |

## `Vec[T]`

A growable array. It has every [`Slice[T]`](#slicet) method, plus:

| Method | Signature | Notes |
|---|---|---|
| `new` | `fn new() -> Vec[T]` | Empty, no allocation |
| `with_capacity` | `fn with_capacity(cap: i64) -> Vec[T]` | Preallocates `cap` slots |
| `filled` | `fn filled(n: i64, val: T) -> Vec[T] where T: Clone` | `n` copies of `val` |
| `from_fn` | `fn from_fn(n: i64, f: own MutFn(i64) -> T) -> Vec[T]` | `n` elements computed from their index |
| `as_slice` | `fn as_slice(self) -> Slice[T]` | The whole vector as a `Slice[T]` value |
| `push` | `fn push(mut ref self, val: own T)` | Appends |
| `pop` | `fn pop(mut ref self) -> Option[T]` | Removes the last element |
| `insert` | `fn insert(mut ref self, idx: i64, val: own T)` | Inserts at `idx`, shifting the rest. Panics if `idx > self.len()` |
| `remove` | `fn remove(mut ref self, idx: i64) -> T` | Removes at `idx`, shifting the rest. Panics if out of bounds |
| `remove_first` | `fn remove_first(mut ref self, pred: own MutFn(T) -> bool) -> Option[T]` | Removes and returns the first element satisfying `pred`. O(n) |
| `retain` | `fn retain(mut ref self, pred: own MutFn(T) -> bool)` | Keeps the elements satisfying `pred` and drops the rest, in one pass |
| `dedup` | `fn dedup(mut ref self) where T: PartialEq` | Drops consecutive equal elements |
| `truncate` | `fn truncate(mut ref self, len: i64)` | Drops the elements from `len` on. Does nothing if `len >= self.len()` |
| `clear` | `fn clear(mut ref self)` | Drops every element |
| `resize` | `fn resize(mut ref self, n: i64, val: own T) where T: Clone` | Sets the length to exactly `n`: truncates when shrinking, appends copies of `val` when growing. `n < 0` counts as 0. `val` is moved into the first new slot and cloned into the rest, and dropped when there is no new slot |
| `extend` | `fn extend[I: IntoIterator[Item = T]](mut ref self, items: own I)` | Appends every item, in order. `a.extend(b)` moves every element of the `Vec` `b` onto the end of `a`. There is no `append` |
| `capacity` | `fn capacity(self) -> i64` | A lower bound, not an allocator promise: `capacity() >= len()` always, and `capacity() >= len() + n` after `reserve(n)`. The exact value is unspecified, so compare with `>=` |
| `reserve` | `fn reserve(mut ref self, additional: i64)` | Makes room for `additional` more elements; it may over-allocate. `additional <= 0` does nothing. Never changes `len()` |
| `reserve_exact` | `fn reserve_exact(mut ref self, additional: i64)` | As `reserve`, but asks for no slack beyond `len() + additional` |

Build a `Vec` from an iterator with `collect` ([iterators.md](iterators.md)).

## `Map[K, V, H]`

`Map[K, V, H = SipHash13BuildHasher]` where `K: Hash + Eq` and `H: BuildHasher`. The third parameter picks the hasher; see [Hashing](#hashing).

| Method | Signature | Notes |
|---|---|---|
| `new` | `fn new() -> Map[K, V, H]` | Empty map |
| `from` | `fn from(pairs: own Vec[(K, V)]) -> Map[K, V, H]` | Builds a map from key-value pairs |
| `insert` | `fn insert(mut ref self, key: own K, val: own V) -> Option[V]` | Returns the previous value if the key was present |
| `get` | `fn get(self, key: K) -> Option[ref V]` | Lookup. For a `Copy` value, `m.get(k) ?? d` supplies a default and yields a `V` (`counts.get(word) ?? 0`); otherwise write `m.get(k).cloned() ?? d` |
| `get_mut` | `fn get_mut(mut ref self, key: K) -> Option[mut ref V]` | Lookup for in-place change |
| `remove` | `fn remove(mut ref self, key: K) -> Option[V]` | Removes and returns the value |
| `contains_key` | `fn contains_key(self, key: K) -> bool` | |
| `len` | `fn len(self) -> i64` | Number of entries |
| `is_empty` | `fn is_empty(self) -> bool` | `self.len() == 0` |
| `keys` | `fn keys(self) -> impl Iterator[Item = ref K]` | |
| `values` | `fn values(self) -> impl Iterator[Item = ref V]` | |
| `entry` | `fn entry(mut ref self, key: own K) -> Entry[K, V]` | The slot for `key`, occupied or vacant |
| `clear` | `fn clear(mut ref self)` | Drops every entry |
| `reserve` | `fn reserve(mut ref self, additional: i64)` | Makes room for `additional` more entries. `additional <= 0` does nothing. There is no `capacity()`: the bucket count is not part of the contract |
| `[]` | `m[key]` | Index operator through `Index`. Panics if the key is missing |
| `[]=` | `m[key] = val` | Index assignment through `IndexSet` ([design.md §9](../design.md#index-and-indexmut)): inserts the entry, or replaces and drops the old value. `m[key] += 1` and `m[key].push(x)` go through `IndexMut` and panic if the key is missing; use `entry` for those |

**`Entry[K, V]`: insert or modify in place.** `entry` gives one lookup with an in-place update path, with no second hash and no key clone:

```kara
enum Entry[K, V] {
    Occupied { value: mut ref V },
    Vacant { key: K, map: mut ref Map[K, V] },
}

impl[K: Hash + Eq, V] Entry[K, V] {
    fn or_insert(own self, default: own V) -> mut ref V
    fn or_insert_with(own self, f: own OnceFn() -> V) -> mut ref V
    fn and_modify(own self, f: own OnceFn(mut ref V)) -> Entry[K, V]
}
```

Append to a per-key `Vec` without cloning:

```kara
self.table.entry(key).or_insert_with(Vec.new).push(row);
```

`or_insert` and `or_insert_with` return a `mut ref V` into the map, which borrows the map ([core-semantics.md §5.4](../core-semantics.md#5-references-and-views-c5)). `and_modify` changes an occupied entry and returns the entry for further chaining.

**Iteration order is unspecified and varies between runs.** The default hasher is seeded per process (see [Hashing](#hashing)), so the order of iteration differs from one run to the next. Code that needs a stable order (sorted output, snapshot tests, reproducible runs) uses `SortedMap`. `Set` behaves the same way.

**Destruction order follows iteration order.** When a `Map` or `Set` drops, every element drops exactly once, in the unspecified iteration order, and within an entry the key drops before the value. `SortedMap` and `SortedSet` drop in key order. See [core-semantics.md §7.8](../core-semantics.md#7-destruction-c6-supersedes-the-drop-judgment-0). A program whose output depends on the order in which `Map` elements run their `Drop` bodies depends on unspecified behaviour.

## `Set[T, H]`

`Set[T, H = SipHash13BuildHasher]` where `T: Hash + Eq`.

| Method | Signature | Notes |
|---|---|---|
| `new` | `fn new() -> Set[T, H]` | Empty set |
| `from` | `fn from(items: own Vec[T]) -> Set[T, H]` | Builds a set from a list; duplicates collapse |
| `insert` | `fn insert(mut ref self, val: own T) -> bool` | `false` if already present |
| `remove` | `fn remove(mut ref self, val: T) -> bool` | `true` if it was present |
| `contains` | `fn contains(self, val: T) -> bool` | Membership test |
| `len` | `fn len(self) -> i64` | Number of elements |
| `is_empty` | `fn is_empty(self) -> bool` | `self.len() == 0` |
| `union` | `fn union(self, other: Set[T, H]) -> Set[T, H]` | |
| `intersection` | `fn intersection(self, other: Set[T, H]) -> Set[T, H]` | |
| `difference` | `fn difference(self, other: Set[T, H]) -> Set[T, H]` | Elements of `self` not in `other` |
| `reserve` | `fn reserve(mut ref self, additional: i64)` | As `Map.reserve` |
| `clear` | `fn clear(mut ref self)` | Drops every element |

## `SortedMap[K, V]`

A B-tree map, `K: Ord`. Iteration is in ascending key order.

| Method | Signature | Notes |
|---|---|---|
| `new` | `fn new() -> SortedMap[K, V]` | Empty map |
| `insert` | `fn insert(mut ref self, key: own K, val: own V) -> Option[V]` | Returns the previous value if the key was present |
| `get` | `fn get(self, key: K) -> Option[ref V]` | |
| `remove` | `fn remove(mut ref self, key: K) -> Option[V]` | |
| `contains_key` | `fn contains_key(self, key: K) -> bool` | |
| `len` | `fn len(self) -> i64` | |
| `is_empty` | `fn is_empty(self) -> bool` | |
| `keys` | `fn keys(self) -> impl Iterator[Item = ref K]` | Ascending |
| `values` | `fn values(self) -> impl Iterator[Item = ref V]` | In key order |
| `range` | `fn range(self, from: K, to: K) -> impl Iterator[Item = (ref K, ref V)]` | Entries with keys in `[from, to)` |
| `clear` | `fn clear(mut ref self)` | |
| `[]` | `m[key]` | Index operator through `Index`. Panics if the key is missing |
| `[]=` | `m[key] = val` | Index assignment through `IndexSet`, as for `Map` |

## `SortedSet[T]`

A B-tree set, `T: Ord`. Every operation is O(log n), and iteration is in ascending order. Use `Set[T]` for membership tests alone; use `SortedSet[T]` for min, max, ordered iteration or range queries.

| Method | Signature | Notes |
|---|---|---|
| `new` | `fn new() -> SortedSet[T]` | Empty set |
| `insert` | `fn insert(mut ref self, val: own T) -> bool` | `false` if already present |
| `remove` | `fn remove(mut ref self, val: T) -> bool` | `true` if it was present |
| `contains` | `fn contains(self, val: T) -> bool` | |
| `min` | `fn min(self) -> Option[ref T]` | Smallest element |
| `max` | `fn max(self) -> Option[ref T]` | Largest element |
| `range` | `fn range(self, from: T, to: T) -> impl Iterator[Item = ref T]` | Elements in `[from, to)`, ascending |
| `len` | `fn len(self) -> i64` | |
| `is_empty` | `fn is_empty(self) -> bool` | |
| `union` | `fn union(self, other: SortedSet[T]) -> SortedSet[T]` | |
| `intersection` | `fn intersection(self, other: SortedSet[T]) -> SortedSet[T]` | |
| `difference` | `fn difference(self, other: SortedSet[T]) -> SortedSet[T]` | |

## `VecDeque[T]`

A ring buffer with O(1) push and pop at both ends.

| Method | Signature | Notes |
|---|---|---|
| `new` | `fn new() -> VecDeque[T]` | Empty deque |
| `push_back` | `fn push_back(mut ref self, val: own T)` | |
| `push_front` | `fn push_front(mut ref self, val: own T)` | |
| `pop_back` | `fn pop_back(mut ref self) -> Option[T]` | |
| `pop_front` | `fn pop_front(mut ref self) -> Option[T]` | |
| `front` | `fn front(self) -> Option[ref T]` | The element `pop_front` would return |
| `back` | `fn back(self) -> Option[ref T]` | The element `pop_back` would return |
| `get` | `fn get(self, idx: i64) -> Option[ref T]` | Index 0 is the front |
| `len` | `fn len(self) -> i64` | |
| `is_empty` | `fn is_empty(self) -> bool` | |
| `clear` | `fn clear(mut ref self)` | |

## `PriorityQueue[T]`

A binary heap, `T: Ord`: a `Vec[T]` arranged so that the next element out is at the root. `push` and `pop` are O(log n); `peek`, `len` and `is_empty` are O(1); building from a vector is O(n).

**Direction.** `PriorityQueue.new()` pops the smallest element first, as Java's `PriorityQueue` and Python's `heapq` do. `PriorityQueue.max_first()` pops the largest first. This is the opposite of Rust's `BinaryHeap`. The direction is a field of the queue, not a second type, so a queue can be passed to generic code without its direction entering the signature.

**Not provided:** `decrease_key` (`push` returns no handle) and a cheap `merge`. Both need a Fibonacci or pairing heap. For ordered iteration over the whole collection, use `SortedSet[T]`.

| Method | Signature | Notes |
|---|---|---|
| `new` | `fn new() -> PriorityQueue[T]` | Empty, smallest first |
| `max_first` | `fn max_first() -> PriorityQueue[T]` | Empty, largest first |
| `from` | `fn from(v: own Vec[T]) -> PriorityQueue[T]` | O(n) build, smallest first |
| `max_first_from` | `fn max_first_from(v: own Vec[T]) -> PriorityQueue[T]` | O(n) build, largest first |
| `push` | `fn push(mut ref self, x: own T)` | O(log n) |
| `peek` | `fn peek(self) -> Option[ref T]` | The next element, without removing it |
| `pop` | `fn pop(mut ref self) -> Option[T]` | Removes and returns the next element |
| `len` | `fn len(self) -> i64` | |
| `is_empty` | `fn is_empty(self) -> bool` | |
| `clear` | `fn clear(mut ref self)` | Drops every element |
| `into_sorted_vec` | `fn into_sorted_vec(own self) -> Vec[T]` | Consumes the queue in pop order: ascending for smallest-first, descending for largest-first |

```kara
let mut pq: PriorityQueue[i64] = PriorityQueue.new();
pq.push(5);
pq.push(1);
pq.push(4);
match pq.peek() { Some(v) => { println(v); } None => {} }  // 1; the queue still holds 3
match pq.pop() { Some(v) => { println(v); } None => {} }   // 1; now it holds 2

let sorted = PriorityQueue.from([9, 7, 8, 1, 3]).into_sorted_vec();   // [1, 3, 7, 8, 9]
```

## Iteration

Every collection works with `for`. A collection is `Iterable`, so `for x in c` borrows `c`; `c.into_iter()` is an `Iterator`, so `for x in c.into_iter()` consumes `c` ([core-semantics.md §4.6](../core-semantics.md#4-parameters-calls-and-patterns)). The iterator methods are:

| Collection | `iter()` item | `iter_mut()` item | `into_iter()` item | Order |
|---|---|---|---|---|
| `Vec[T]`, `Array[T, N]`, `Slice[T]` | `ref T` | `mut ref T` | `T` | Index order |
| `Map[K, V]` | `(ref K, ref V)` | `(ref K, mut ref V)` | `(K, V)` | Unspecified |
| `Set[T]` | `ref T` | none | `T` | Unspecified |
| `VecDeque[T]` | `ref T` | `mut ref T` | `T` | Front to back |
| `SortedMap[K, V]` | `(ref K, ref V)` | `(ref K, mut ref V)` | `(K, V)` | Ascending key |
| `SortedSet[T]` | `ref T` | none | `T` | Ascending |

`Set` and `SortedSet` have no `iter_mut`, because changing an element in place would break the hash or the ordering. `iter()` borrows the collection, `iter_mut()` borrows it mutably for the whole iteration, and `into_iter()` consumes it. Each returns `impl Iterator` with the item shown.

## Common traits

`Vec`, `Map`, `Set`, `VecDeque`, `SortedMap` and `SortedSet` implement `Clone`. Every collection implements `Display` when its elements implement `Display`, and `FromIterator` for its natural element type, so `collect` can build it. `Display` adds only punctuation, never a type name ([design.md §9](../design.md#9-traits)): `Vec`, `Array`, `Slice` and `VecDeque` render as `[a, b]`, `Map` and `SortedMap` as `{k: v, k2: v2}`, and `Set` and `SortedSet` as `{a, b}`; empty ones render as `[]` or `{}`.

`Vec[T]`, `Array[T, N]`, `Slice[T]` and `VecDeque[T]` implement `PartialEq` (with `Rhs = Self`) when `T: PartialEq`, and `Eq` when `T: Eq`: two sequences are equal when they have the same length and equal elements in order. They implement `PartialOrd` and `Ord` when `T` does, comparing lexicographically. The comparison traits borrow both operands ([design.md §9](../design.md#operator-traits)).

## `Arena[T]` and `Pool[T]`

Two allocation primitives are library types, not language features:
- **`Arena[T]`** is a bulk allocator with one lifetime. Every item is freed together when the arena drops, which suits cache-friendly batch work such as parse trees and per-frame game data. Items cannot be removed one at a time. Not yet implemented.
- **`Pool[T]`** is a generational-handle allocator with a lifetime per item. Items are inserted and removed one at a time; `pool.insert(x)` returns a `Handle[T]`, and `pool.get(h)` returns `Option[ref T]`, which is `None` when the handle is stale. Use it when items have independent lifetimes and stale-handle detection matters (entities, connection tables), and for self-referential data that is deduplicated rather than shared: a type table holds a `Pool[Type]`, and the recursive positions hold `Handle[Type]` ([design.md §11](../design.md#cycles-and-weak)).

## Fallible allocation

An allocation can fail at run time: the kernel refuses memory, a fixed heap region is full, or a requested capacity is too large to represent. Every collection method that allocates comes in two forms: the plain method, which panics on allocation failure, and a `try_*` twin that returns the failure as a value.

**Default behaviour.** Every allocating method (`Vec.push`, `Vec.insert`, `Vec.extend`, `Vec.reserve`, `Vec.with_capacity`, `String.push`, `String.push_str`, `Map.insert`, `Set.insert` and the rest) panics when the allocation fails ([core-semantics.md §10](../core-semantics.md#10-panics-and-errors-c8)). Most code cannot do anything useful when memory runs out, so this is the default. That panic is covered by the method's `allocates(Heap)` effect, not by a separate `panics` ([design.md §12](../design.md#default-permitted-effects)).

**Fallible twins.** Each panicking method has a `try_*` companion. Operations that report only success or failure return `Result[(), AllocError]` (`try_push`, `try_insert`, `try_reserve`, `try_extend`). Constructors and capacity-bearing operations return `Result[T, AllocError]` (`try_with_capacity`, `try_new`).

```kara
// Panics on allocation failure
let mut v: Vec[i64] = Vec.new();
v.push(42);

// Returns the failure as a value
let mut w: Vec[i64] = Vec.try_with_capacity(1024)?;
w.try_push(42)?;
```

The two forms are independent, and the fallible one is always available. A caller that wants explicit handling at one site calls `try_push(x)?` there.

**`AllocError`** is a prelude type:

```kara
enum AllocError {
    OutOfMemory { requested_bytes: i64 },
    CapacityOverflow,
}
```

`OutOfMemory` carries the byte count the allocator could not satisfy, for diagnostics, retrying with a smaller request, or telemetry. `CapacityOverflow` means the requested capacity could not be represented, so the allocator was never asked; it is a logic error that code should catch at its input boundary by validating sizes before allocating. `AllocError` implements `Debug`, `Display`, `Eq` and `Copy`. Its `Display` names the byte count for `OutOfMemory` and prints "capacity overflow" for `CapacityOverflow`. A `Result[T, AllocError]` propagates through `?` like any other `Result`; a function that calls `try_*` methods converts `AllocError` into its own error type with `From`.

**Methods and their twins:**

| Type | Panicking method | Fallible twin |
|---|---|---|
| `Vec[T]` | `new()` | `new()` (does not allocate) |
|  | `with_capacity(n)` | `try_with_capacity(n)` |
|  | `push(x)` | `try_push(x)` |
|  | `insert(i, x)` | `try_insert(i, x)` |
|  | `extend(items)` | `try_extend(items)` |
|  | `reserve(n)` | `try_reserve(n)` |
|  | `reserve_exact(n)` | `try_reserve_exact(n)` |
|  | `resize(n, val)` | `try_resize(n, val)` |
|  | `clone()` | `try_clone()` |
|  | `collect()` into a `Vec` | `Vec.try_from_iter(iter)` |
| `String` | `new()` | `new()` (does not allocate) |
|  | `push(c)` | `try_push(c)` |
|  | `push_str(s)` | `try_push_str(s)` |
|  | `with_capacity(n)` | `try_with_capacity(n)` |
|  | `reserve(n)` | `try_reserve(n)` |
|  | `clone()` | `try_clone()` |
| `Map[K, V]`, `Set[T]`, `VecDeque[T]`, `SortedSet[T]` | `new()` | `new()` (does not allocate) |
|  | `with_capacity(n)` (where present) | `try_with_capacity(n)` |
|  | `insert(...)` | `try_insert(...)` |
|  | `reserve(n)` (where present) | `try_reserve(n)` |
|  | `clone()` | `try_clone()` |

**Why two methods, not one return type that depends on configuration.** A return type that changed with project configuration (`fn push(...)` returning `()` in one build and `Result[(), AllocError]` in another) would break generic code: a function calling `v.push(x)` could not compile both ways, and library code would split into two trees. With two methods, the caller picks the form it needs and generic code calls that form: `fn fill[T: Clone](v: mut ref Vec[T], n: i64, val: T) -> Result[(), AllocError]` calls `try_push` and compiles the same everywhere.

A project-wide switch that rejects the panicking forms is part of the systems track ([deferred.md](../deferred.md#systems)).

## Hashing

`Map` and `Set` hash their keys through two traits. `Hash` (a type's way of feeding itself to a hasher) and `derive(Hash)` are in [design.md §9](../design.md#9-traits), together with the rule that equal values must feed identical bytes. The other half, in `std.hash`, is the hasher:

```kara
trait Hasher {
    fn write(mut ref self, bytes: Slice[u8]);
    fn finish(self) -> u64;

    // Provided methods that feed fixed-width integers through `write`:
    fn write_u8(mut ref self, n: u8)    { self.write([n]); }
    fn write_u16(mut ref self, n: u16)  { self.write(n.to_ne_bytes()); }
    fn write_u32(mut ref self, n: u32)  { self.write(n.to_ne_bytes()); }
    fn write_u64(mut ref self, n: u64)  { self.write(n.to_ne_bytes()); }
    // and the same for the other integer widths
}

trait BuildHasher {
    type Hasher: Hasher;
    fn build(self) -> Self.Hasher;
}
```

The split lets the hash algorithm be a property of the table, not of the key: `Map[K, V, FxBuildHasher]` and `Map[K, V, SipHash13BuildHasher]` use the same `Hash` impls for `K`. A `BuildHasher` makes a fresh `Hasher` for each hash, so per-hash state (the `Hasher`) stays separate from per-table configuration (the builder).

**Hashers the library provides.**
- `SipHash13BuildHasher` is the default for `Map[K, V]` and `Set[T]`. It is resistant to hash flooding and keyed from a random value chosen once per process. The default may change in a future Kāra version if a better algorithm of the same kind becomes standard.
- `FxBuildHasher` is fast and unkeyed. A table that names it gives up the protection against hash flooding.

Both are ordinary `BuildHasher` types written in Kāra, like a user hasher, and every backend runs the same implementation.

**A user hasher** is any type that implements `BuildHasher`, named in the table's last type parameter: `Map[K, V, MyBuildHasher]`, `Set[T, MyBuildHasher]`. The builder must be a struct with no fields, because the table names its builder as a type and constructs it itself; a seed belongs inside `build()`. A field-carrying builder is a compile error.

**Keys use their own impls.** A table hashes each key through the key type's `Hash` impl and compares keys through its `PartialEq` impl (which `Eq` requires), whether derived or written by hand. So an impl that deliberately ignores a field puts two keys that differ only in that field in the same slot. `Hash` decides which bytes a key contributes and the table's `BuildHasher` decides how those bytes become a digest, so a hand-written `Hash` composes with any hasher.

**Stability policy.** The default hasher's values are not stable across runs, across Kāra versions or across targets. This matches Python and modern Java: the common use is in-memory keying, and the common attack is hash flooding from adversarial input. Iteration order changes with them (see [`Map`](#mapk-v-h)). For testing, the environment variable `KARAC_HASH_SEED` pins the per-process key so that a run can be reproduced. It is a testing aid only: a deployment must not set it, since a known key gives up the protection the default exists for.

**Stable digests use `StableHash`, not `Hash`.** Content addressing, on-disk indexes, snapshot tests and sharding need a digest that never changes. `StableHash` provides explicit, versioned functions for this. They take bytes, name the algorithm in the function name, take the key as an argument, and are versioned separately from the `Hash` trait. `karac explain --concept=stable-hash` points users who reach for `Hash` here.

```kara
fn content_id(payload: Str) -> u64 {
    StableHash.siphash24(payload.bytes(), 0, 0)
}
```

`StableHash.siphash24(bytes: Slice[u8], k0: u64, k1: u64) -> u64` computes SipHash-2-4 over `bytes` under the caller's 128-bit key and reads no process state. The same bytes under the same key give the same value across runs, versions, targets and machines. It uses 2-4 rounds, the count the SipHash paper specifies, so that it agrees with other languages' `siphash24`. The key is required because changing it changes every stored digest, and that belongs at the call site. `StableHash` is a namespace that is never constructed.

**`StableHash` is not a cryptographic hash.** SipHash is a fast keyed function, not collision-resistant, and unfit for signatures or for deduplication against an adversary. Cryptographic hashing belongs to `std.crypto`, which is not yet available; its general hash is BLAKE3 ([deferred.md](../deferred.md)). Until it lands, a cryptographic hash comes from an external library.

**There is no `xxh3`.** A fast digest for content addressing is better served by BLAKE3, which is in the same speed class and also collision-resistant. A third stable-hash option that resists neither flooding nor collisions would add a choice every user must get right, for no new capability.
