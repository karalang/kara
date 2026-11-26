# collections.md sentence-to-cell map

One row per normative unit of docs/library/collections.md (each prose sentence, each table row, each code block), as of 2026-11-25, naming the cell in this directory (or the test elsewhere) that covers it. Run with `python3 conformance/cases.py conformance/library/collections`; every cell passes on the interpreter and the compiled build. `col1` to `col4` and `arr_*` are the 2026-11-20 cells moved here (`col1` checked against a Python mirror of the slice methods, `col4` against a reference SipHash-1-3 and SipHash-2-4); every other cell is new in this audit. Each expectation was checked live: with every `.out`, `.err`, `.refused`, `.exit` and `.effects` perturbed, each of the 162 cells fails. FIXED marks a sentence the compiler broke until this audit (B-2026-11-25-28, B-2026-11-25-29); nothing here FAILS.

| # | Section | Sentence | Cell(s) |
|---|---|---|---|
| col.1 | Collections | The standard collections are library types written in Kāra, not language primitives. | rationale, no cell |
| col.2 | Collections | row: `Array[T, N]` / `[T; N]` / Fixed-size array | the type's own section below |
| col.3 | Collections | row: `Vec[T]` / `Vec<T>` / Dynamic array | the type's own section below |
| col.4 | Collections | row: `Slice[T]` / `&[T]` / `&mut [T]` / Borrowed view into contiguous memory ([design.md §5](../design.md#5-types)) | the type's own section below |
| col.5 | Collections | row: `Map[K, V]` / `HashMap<K, V>` / Key-value lookup, `K: Hash + Eq` | the type's own section below |
| col.6 | Collections | row: `Set[T]` / `HashSet<T>` / Unique values, `T: Hash + Eq` | the type's own section below |
| col.7 | Collections | row: `SortedMap[K, V]` / `BTreeMap<K, V>` / Key-value lookup in key order, `K: Ord` | the type's own section below |
| col.8 | Collections | row: `SortedSet[T]` / `BTreeSet<T>` / Unique values in ascending order, `T: Ord`; min, max and range queries | the type's own section below |
| col.9 | Collections | row: `VecDeque[T]` / `VecDeque<T>` / Double-ended queue (ring buffer) | the type's own section below |
| col.10 | Collections | row: `PriorityQueue[T]` / `BinaryHeap<T>` / Binary heap, `T: Ord`. Smallest-first by default, the opposite of Rust's `BinaryHeap` | the type's own section below |
| col.11 | Collections | `String` and `Str` are in [strings.md](strings.md); `Option` and `Result` are in [core-types.md](core-types.md). | cross-reference, no cell |
| col.12 | Collections | **Effects.** A method that grows or reallocates a collection allocates, and a method noted "panics if ..." can panic. | effects_alloc_and_panics_are_default_permitted |
| col.13 | Collections | Both effects are default-permitted ([README](README.md#signature-conventions)), so the signatures below omit them. | effects_alloc_and_panics_are_default_permitted |
| col.14 | Collections | A method that takes a function parameter has that argument's effects at each call. | effects_function_argument_effects_at_each_call |
| col.15 | Collections | **Element traits.** A method that runs a trait on the values it holds has the effects of that trait's impl for the element type at the call, | effects_element_trait_impls_are_charged (sort/contains on a program type charged; Map/SortedSet len charged; a Map's values never hashed) |
| col.16 | Collections | For the built-in element types that is nothing; for a program type it is whatever its impl does. | effects_element_trait_impls_are_charged (sort/contains on a program type charged; Map/SortedSet len charged; a Map's values never hashed) |
| col.17 | Collections | The signatures below say which trait each method runs: | effects_element_trait_impls_are_charged (sort/contains on a program type charged; Map/SortedSet len charged; a Map's values never hashed) |
| col.18 | Collections | A method bounded on the element (`where T: PartialEq`, `where T: Ord`, `where T: Clone`, `where T: Display`, `[K: Ord]`) runs that trait's m | effects_element_trait_impls_are_charged (sort/contains on a program type charged; Map/SortedSet len charged; a Map's values never hashed) |
| col.19 | Collections | `contains`, `starts_with`, `ends_with` and `dedup` run `eq`; `sort`, `binary_search` and `is_sorted` run `cmp`, and `sort_by_key` the key's  | effects_element_trait_impls_are_charged (sort/contains on a program type charged; Map/SortedSet len charged; a Map's values never hashed) |
| col.20 | Collections | A `Map` or `Set` method has the effects of its key's `hash` and `eq`, and of the hasher's methods when `H` is a program type; a `SortedMap`, | effects_element_trait_impls_are_charged (sort/contains on a program type charged; Map/SortedSet len charged; a Map's values never hashed) |
| col.21 | Collections | The types are declared with the bound (`Map[K, V]` where `K: Hash + Eq`), so that is every method of the five, `m[key]`, `from`, a literal a | effects_element_trait_impls_are_charged (sort/contains on a program type charged; Map/SortedSet len charged; a Map's values never hashed) |
| col.22 | Collections | A `Map`'s values are never hashed or ordered. | effects_element_trait_impls_are_charged (sort/contains on a program type charged; Map/SortedSet len charged; a Map's values never hashed) |
| col.23 | Collections | `==`, `!=`, the ordering operators, `clone()`, `Display` and `Debug` on a collection ([Common traits](#common-traits)) run the same trait on | effects_element_trait_impls_are_charged (sort/contains on a program type charged; Map/SortedSet len charged; a Map's values never hashed) |
| col.24 | Collection literals | The literal forms (`[a, b]`, `[v; n]`, `[k: v]`, `[]` and `[:]`), and how an expected type picks the collection a literal builds, are in [de | cross-reference to design.md §5 collection literals (conformance/design/types) |
| col.25 | Collection literals | Where no expected type is available, and for the collections no literal names, the `from` constructors build a collection from a list: | from_constructors_build_each_collection |
| col.26 | Collection literals | code: let primes = Set.from([2, 3, 5, 7]);                 // Set[i64] let ages = Map.from([("ann", 31), ("bo", 27)]);      // Map[String, i | from_constructors_build_each_collection |
| col.27 | `Array[T, N]` | A fixed-size array of `N` elements, stored inline. | array_has_slice_methods_as_slice_and_indexing; arr_a1 to arr_a24 (2026-11-20) |
| col.28 | `Array[T, N]` | It has every [`Slice[T]`](#slicet) method, `as_slice(self) -> Slice[T]`, and indexing. | array_has_slice_methods_as_slice_and_indexing; arr_a1 to arr_a24 (2026-11-20) |
| col.29 | `Array[T, N]` | An `Array[T, N]` argument passes as `Slice[T]`. | array_has_slice_methods_as_slice_and_indexing; arr_a1 to arr_a24 (2026-11-20) |
| col.30 | `Array[T, N]` | **Definite assignment.** `let arr: Array[T, N];` without an initializer reserves the storage and initializes nothing. | array_read_before_whole_assignment_refused |
| col.31 | `Array[T, N]` | Reading `arr` before a whole-value assignment is a compile error, as for any binding ([design.md §6](../design.md#6-expressions-and-statemen | array_read_before_whole_assignment_refused |
| col.32 | `Array[T, N]` | Assigning every slot one at a time does not count: the analysis tracks whole-value assignment only. | array_slot_by_slot_assignment_does_not_count_refused |
| col.33 | `Array[T, N]` | To fill slot by slot, initialize the whole array first (`let mut arr: Array[i64, 8] = [0; 8];`) and overwrite, or build it with `Array.from_ | array_whole_init_then_overwrite_and_from_fn |
| col.34 | `Slice[T]` | `Slice[T]` is the borrowed view of a contiguous sequence. | cross-reference, no cell |
| col.35 | `Slice[T]` | Its type and borrow rules are in [design.md §5](../design.md#slices). | cross-reference, no cell |
| col.36 | `Slice[T]` | A `Vec[T]` or `Array[T, N]` receiver finds every method below: method lookup searches the type's own methods first, then `Slice[T]`'s, throu | slice_methods_found_on_vec_and_array_receivers; col1 |
| col.37 | `Slice[T]` | **Reading.** | heading |
| col.38 | `Slice[T]` | row: `len` / `fn len(self) -> i64` / Number of elements | slice_len_get_first_last_contains; col1 |
| col.39 | `Slice[T]` | row: `is_empty` / `fn is_empty(self) -> bool` / `self.len() == 0` | slice_len_get_first_last_contains; col1 |
| col.40 | `Slice[T]` | row: `get` / `fn get(self, idx: i64) -> Option[ref T]` / `None` if `idx` is out of bounds. `v[i]` panics instead | slice_len_get_first_last_contains, slice_index_out_of_bounds_panics; col1 |
| col.41 | `Slice[T]` | row: `get_unchecked` / `unsafe fn get_unchecked(self, idx: i64) -> ref T` / No bounds check: the caller ensures `0 <= idx < self.len()`, and | slice_get_unchecked_in_unsafe, slice_get_unchecked_outside_unsafe_refused (out-of-range is undefined behaviour, no cell) |
| col.42 | `Slice[T]` | row: `first` / `fn first(self) -> Option[ref T]` / `None` if empty | slice_len_get_first_last_contains; col1 |
| col.43 | `Slice[T]` | row: `last` / `fn last(self) -> Option[ref T]` / `None` if empty | slice_len_get_first_last_contains; col1 |
| col.44 | `Slice[T]` | row: `contains` / `fn contains(self, val: T) -> bool where T: PartialEq` / Linear search | slice_len_get_first_last_contains; col1 |
| col.45 | `Slice[T]` | row: `starts_with` / `fn starts_with(self, prefix: Slice[T]) -> bool where T: PartialEq` / Whether the first `prefix.len()` elements equal ` | slice_starts_with_ends_with; col1 |
| col.46 | `Slice[T]` | row: `ends_with` / `fn ends_with(self, suffix: Slice[T]) -> bool where T: PartialEq` / Whether the last `suffix.len()` elements equal `suffi | slice_starts_with_ends_with; col1 |
| col.47 | `Slice[T]` | row: `binary_search` / `fn binary_search(self, val: T) -> Option[i64] where T: Ord` / Index of an element equal to `val`, or `None`. The seq | slice_binary_search_and_partition_point; col1 |
| col.48 | `Slice[T]` | row: `partition_point` / `fn partition_point(self, pred: own MutFn(T) -> bool) -> i64` / Index of the first element `pred` rejects, by binar | slice_binary_search_and_partition_point; col1 |
| col.49 | `Slice[T]` | row: `is_sorted` / `fn is_sorted(self) -> bool where T: Ord` / Non-strict ascending check: equal neighbours count as sorted, and 0 or 1 elem | slice_is_sorted_non_strict; col1 |
| col.50 | `Slice[T]` | row: `split_at` / `fn split_at(self, mid: i64) -> (Slice[T], Slice[T])` / `[0, mid)` and `[mid, len)`. Panics if `mid > self.len()` | slice_split_at, slice_split_at_past_len_panics |
| col.51 | `Slice[T]` | row: `windows` / `fn windows(self, size: i64) -> Vec[Slice[T]]` / Every run of `size` consecutive elements. Panics with `windows: size must  | slice_windows_and_chunks, slice_windows_non_positive_panics, slice_windows_and_chunks_walked, slice_windows_walked_non_positive_panics; tests/collections_conformance.rs coll_windows_and_chunks_of_a_non_positive_size_say_so |
| col.52 | `Slice[T]` | row: `chunks` / `fn chunks(self, size: i64) -> Vec[Slice[T]]` / Consecutive runs of `size` elements; the last may be shorter. Panics with `c | slice_windows_and_chunks, slice_chunks_non_positive_panics, slice_windows_and_chunks_walked, slice_chunks_walked_non_positive_panics |
| col.53 | `Slice[T]` | row: `split` / `fn split(self, pred: own MutFn(T) -> bool) -> Vec[Slice[T]]` / The runs between elements `pred` accepts, which are dropped:  | slice_split_and_chunk_by |
| col.54 | `Slice[T]` | row: `chunk_by` / `fn chunk_by(self, same: own MutFn(T, T) -> bool) -> Vec[Slice[T]]` / Consecutive runs in which `same` accepts each adjace | slice_split_and_chunk_by |
| col.55 | `Slice[T]` | row: `to_vec` / `fn to_vec(self) -> Vec[T] where T: Clone` / Owned copy | slice_to_vec_and_try_into_array |
| col.56 | `Slice[T]` | row: `try_into_array` / `fn try_into_array(self) -> Option[Array[T, N]] where T: Copy` / The elements copied into an array, `None` when the  | slice_to_vec_and_try_into_array, slice_try_into_array_with_no_expected_type_refused (the `karac fix` rewrite of Rust's try_into is tests/rust_receiver_fixes.rs) |
| col.57 | `Slice[T]` | row: `join` / `fn join(self, sep: Str) -> String where T: Display` / Displays each element, separated by `sep` | slice_join_and_concat; arr_a1 (Array[String].join) |
| col.58 | `Slice[T]` | row: `concat` / `fn concat(self) -> Vec[U]` for `T = Vec[U]`, `Slice[U]` or `Array[U, N]`; `-> String` for `T = String` or `Str` / A sequenc | slice_join_and_concat (the deprecated Display join is tests/collections_conformance.rs) |
| col.59 | `Slice[T]` | row: `[]` / `v[i]` and `v[a..b]` / Index operator through `Index` ([design.md §9](../design.md#9-traits)). `v[i]` is `ref T` and panics if o | slice_index_and_range, slice_index_out_of_bounds_panics |
| col.60 | `Slice[T]` | **Changing in place.** On a `Slice[T]` these need a `mut Slice[T]`. | slice_sort_on_a_mut_slice_view, slice_sort_on_a_shared_slice_refused |
| col.61 | `Slice[T]` | row: `sort` / `fn sort(mut ref self) where T: Ord` / Stable sort, ascending | slice_sorts_are_stable |
| col.62 | `Slice[T]` | row: `sort_by` / `fn sort_by(mut ref self, cmp: own MutFn(T, T) -> Ordering)` / Stable sort by a comparison: `w.sort_by(f64.total_cmp)` | slice_sorts_are_stable |
| col.63 | `Slice[T]` | row: `sort_by_key` / `fn sort_by_key[K: Ord](mut ref self, key: own MutFn(T) -> K)` / Stable sort by a derived key | slice_sorts_are_stable |
| col.64 | `Slice[T]` | row: `reverse` / `fn reverse(mut ref self)` / Reverses in place | slice_reverse_fill_swap, slice_sort_on_a_mut_slice_view |
| col.65 | `Slice[T]` | row: `fill` / `fn fill(mut ref self, val: T) where T: Clone` / Sets every element to a copy of `val` | slice_reverse_fill_swap; tests/collections_conformance.rs coll_filled_resize_fill_call_a_user_clone_and_move_into_the_last_slot |
| col.66 | `Slice[T]` | row: `swap` / `fn swap(mut ref self, i: i64, j: i64)` / Exchanges the elements at `i` and `j`. Neither value is destroyed, so no `Drop` body | slice_reverse_fill_swap, slice_swap_runs_no_drop, slice_swap_out_of_bounds_panics, slice_three_line_swap_of_non_copy_refused |
| col.67 | `Slice[T]` | row: `rotate_left` / `fn rotate_left(mut ref self, mid: i64)` / Turns the elements `mid` places toward the front: the element at `mid` comes | slice_rotate_left_right, slice_rotate_left_past_len_panics |
| col.68 | `Slice[T]` | row: `rotate_right` / `fn rotate_right(mut ref self, k: i64)` / Turns the elements `k` places toward the back: the last `k` come first. Pani | slice_rotate_left_right, slice_rotate_right_negative_panics |
| col.69 | `Slice[T]` | row: `clone_from_slice` / `fn clone_from_slice(mut ref self, src: Slice[T]) where T: Clone` / Overwrites every element with a clone of the m | slice_clone_from_slice_and_range_assign, slice_clone_from_slice_length_mismatch_panics |
| col.70 | `Slice[T]` | row: `split_at_mut` / `fn split_at_mut(mut ref self, mid: i64) -> (mut Slice[T], mut Slice[T])` / Two disjoint mutable halves, `[0, mid)` an | slice_split_at_mut_halves_live_at_once, slice_split_at_mut_past_len_panics, slice_split_at_mut_sequence_used_while_half_live_refused (parallel writes: conformance/concurrency) |
| col.71 | `Slice[T]` | row: `[]=` / `v[i] = x` / Index assignment through `IndexMut` ([design.md §9](../design.md#index-and-indexmut)). Panics if out of bounds | slice_clone_from_slice_and_range_assign, slice_index_assign_out_of_bounds_panics |
| col.72 | `Slice[T]` | row: `[a..b]=` / `v[a..b] = other` / Copies the elements of `other` into the range. Requires `T: Copy`; panics if the lengths differ. For ot | slice_clone_from_slice_and_range_assign, slice_range_assign_length_mismatch_panics, slice_range_assign_of_non_copy_refused |
| col.73 | `Slice[T]` | The sorts guarantee their result (sorted, and stable where the table says so), not how they get there: the number and order of calls to `cmp | slice_sorts_are_stable (the call count is unspecified, no cell) |
| col.74 | `Slice[T]` | A comparator that is not a total order leaves the elements in some unspecified permutation, and never panics or loses an element. | slice_sort_with_non_total_comparator_keeps_every_element |
| col.75 | `Vec[T]` | A growable array. | vec_* cells; col1 |
| col.76 | `Vec[T]` | It has every [`Slice[T]`](#slicet) method, plus: | vec_* cells; col1 |
| col.77 | `Vec[T]` | row: `new` / `fn new() -> Vec[T]` / Empty, no allocation | vec_constructors; col1 |
| col.78 | `Vec[T]` | row: `with_capacity` / `fn with_capacity(cap: i64) -> Vec[T]` / Preallocates `cap` slots. `cap < 0` counts as 0, as in `filled` | vec_constructors; col1 |
| col.79 | `Vec[T]` | row: `filled` / `fn filled(n: i64, val: T) -> Vec[T] where T: Clone` / `n` copies of `val`. `n < 0` counts as 0 | vec_constructors; col1 |
| col.80 | `Vec[T]` | row: `from_fn` / `fn from_fn(n: i64, f: own MutFn(i64) -> T) -> Vec[T]` / `n` elements computed from their index. `n < 0` counts as 0, as in | vec_constructors; col1 |
| col.81 | `Vec[T]` | row: `as_slice` / `fn as_slice(self) -> Slice[T]` / The whole vector as a `Slice[T]` value | vec_constructors; col1 |
| col.82 | `Vec[T]` | row: `as_ptr` / `fn as_ptr(self) -> *const T` / The address of element 0, for FFI ([design.md §15](../design.md#15-unsafe-ffi-and-layout-con | vec_as_ptr_and_as_mut_ptr |
| col.83 | `Vec[T]` | row: `push` / `fn push(mut ref self, val: own T)` / Appends | vec_push_pop_insert_remove |
| col.84 | `Vec[T]` | row: `pop` / `fn pop(mut ref self) -> Option[T]` / Removes the last element | vec_push_pop_insert_remove |
| col.85 | `Vec[T]` | row: `insert` / `fn insert(mut ref self, idx: i64, val: own T)` / Inserts at `idx`, shifting the rest. Panics if `idx > self.len()` | vec_push_pop_insert_remove, vec_insert_past_len_panics |
| col.86 | `Vec[T]` | row: `remove` / `fn remove(mut ref self, idx: i64) -> T` / Removes at `idx`, shifting the rest. Panics if out of bounds | vec_push_pop_insert_remove, vec_remove_out_of_bounds_panics |
| col.87 | `Vec[T]` | row: `remove_first` / `fn remove_first(mut ref self, pred: own MutFn(T) -> bool) -> Option[T]` / Removes and returns the first element satis | vec_remove_first_retain_dedup; col1 |
| col.88 | `Vec[T]` | row: `retain` / `fn retain(mut ref self, pred: own MutFn(T) -> bool)` / Keeps the elements satisfying `pred` and drops the rest, in one pass | vec_remove_first_retain_dedup; col1 |
| col.89 | `Vec[T]` | row: `dedup` / `fn dedup(mut ref self) where T: PartialEq` / Drops consecutive equal elements | vec_remove_first_retain_dedup; col1 |
| col.90 | `Vec[T]` | row: `truncate` / `fn truncate(mut ref self, len: i64)` / Drops the elements from `len` on. Does nothing if `len >= self.len()`; `len < 0` c | vec_truncate_and_clear |
| col.91 | `Vec[T]` | row: `drain` / `fn drain(mut ref self, r: Range[i64]) -> VecIntoIter[T]` / Removes the elements at `start..end` (the end excluded), shifting | vec_drain_removes_at_once, vec_drain_past_end_panics, vec_drain_reversed_panics, vec_drain_rust_range_forms_refused |
| col.92 | `Vec[T]` | row: `split_off` / `fn split_off(mut ref self, at: i64) -> Vec[T]` / Moves the elements from `at` on into a new `Vec` and returns it, leavin | vec_split_off, vec_split_off_past_len_panics |
| col.93 | `Vec[T]` | row: `clear` / `fn clear(mut ref self)` / Drops every element | vec_truncate_and_clear |
| col.94 | `Vec[T]` | row: `resize` / `fn resize(mut ref self, n: i64, val: own T) where T: Clone` / Sets the length to exactly `n`: truncates when shrinking, app | vec_resize_clones_then_moves_last; tests/collections_conformance.rs coll_filled_resize_fill_call_a_user_clone_and_move_into_the_last_slot |
| col.95 | `Vec[T]` | row: `extend` / `fn extend[I: IntoIterator[Item = T]](mut ref self, items: own I)` / Appends every item, in order. `a.extend(b)` moves every | vec_extend_and_extend_from_slice, vec_extend_moves_its_argument, vec_has_no_append_refused |
| col.96 | `Vec[T]` | row: `extend_from_slice` / `fn extend_from_slice(mut ref self, items: Slice[T]) where T: Clone` / Appends a clone of every element of `items | vec_extend_and_extend_from_slice |
| col.97 | `Vec[T]` | row: `capacity` / `fn capacity(self) -> i64` / A lower bound, not an allocator promise: `capacity() >= len()` always, and `capacity() >= len | vec_capacity_and_reserve |
| col.98 | `Vec[T]` | row: `reserve` / `fn reserve(mut ref self, additional: i64)` / Makes room for `additional` more elements; it may over-allocate. `additional  | vec_capacity_and_reserve |
| col.99 | `Vec[T]` | row: `reserve_exact` / `fn reserve_exact(mut ref self, additional: i64)` / As `reserve`, but asks for no slack beyond `len() + additional` | vec_capacity_and_reserve |
| col.100 | `Vec[T]` | Build a `Vec` from an iterator with `collect` ([iterators.md](iterators.md)). | vec_built_by_collect |
| col.101 | `Map[K, V, H]` | `Map[K, V, H = SipHash13BuildHasher]` where `K: Hash + Eq` and `H: BuildHasher`. | hashing_builders_build_hashers_that_match_the_table, hashing_user_build_hasher |
| col.102 | `Map[K, V, H]` | The third parameter picks the hasher; see [Hashing](#hashing). | hashing_builders_build_hashers_that_match_the_table, hashing_user_build_hasher |
| col.103 | `Map[K, V, H]` | The bound holds wherever a table type is written, not only where a table is built. | map_bound_on_type_parameter_needed_refused_and_fixed (refused, and `karac fix` adds the bound) |
| col.104 | `Map[K, V, H]` | A key that is a type parameter needs the bound on the item that declares the parameter: | map_bound_on_type_parameter_needed_refused_and_fixed (refused, and `karac fix` adds the bound) |
| col.105 | `Map[K, V, H]` | `struct Matcher[T] { b2j: Map[T, Vec[i64]] }` is an error, and `struct Matcher[T: Hash + Eq]` is required there and on every impl or functio | map_bound_on_type_parameter_needed_refused_and_fixed (refused, and `karac fix` adds the bound) |
| col.106 | `Map[K, V, H]` | `karac fix` adds the bound. | map_bound_on_type_parameter_needed_refused_and_fixed (refused, and `karac fix` adds the bound) |
| col.107 | `Map[K, V, H]` | The same holds for `Set[T]`. | map_bound_on_type_parameter_needed_refused_and_fixed (refused, and `karac fix` adds the bound) |
| col.108 | `Map[K, V, H]` | row: `new` / `fn new() -> Map[K, V, H]` / Empty map | map_methods; col2 |
| col.109 | `Map[K, V, H]` | row: `from` / `fn from(pairs: own Vec[(K, V)]) -> Map[K, V, H]` / Builds a map from key-value pairs | map_methods; col2 |
| col.110 | `Map[K, V, H]` | row: `insert` / `fn insert(mut ref self, key: own K, val: own V) -> Option[V]` / Returns the previous value if the key was present | map_methods; col2 |
| col.111 | `Map[K, V, H]` | row: `extend` / `fn extend[I: IntoIterator[Item = (K, V)]](mut ref self, items: own I)` / Inserts every pair, in order; a later pair replace | map_methods; col2 |
| col.112 | `Map[K, V, H]` | row: `get` / `fn get(self, key: K) -> Option[ref V]` / Lookup. For a `Copy` value, `m.get(k) ?? d` supplies a default and yields a `V` (`cou | map_methods (`?? 0` on a Copy value), map_get_cloned_default_for_non_copy; col2 |
| col.113 | `Map[K, V, H]` | row: `get_mut` / `fn get_mut(mut ref self, key: K) -> Option[mut ref V]` / Lookup for in-place change | map_methods; col2 |
| col.114 | `Map[K, V, H]` | row: `remove` / `fn remove(mut ref self, key: K) -> Option[V]` / Removes and returns the value | map_methods; col2 |
| col.115 | `Map[K, V, H]` | row: `contains_key` / `fn contains_key(self, key: K) -> bool` | map_methods; col2 |
| col.116 | `Map[K, V, H]` | row: `len` / `fn len(self) -> i64` / Number of entries | map_methods; col2 |
| col.117 | `Map[K, V, H]` | row: `is_empty` / `fn is_empty(self) -> bool` / `self.len() == 0` | map_methods; col2 |
| col.118 | `Map[K, V, H]` | row: `keys` / `fn keys(self) -> impl Iterator[Item = ref K]` | map_keys_values_into; col2 |
| col.119 | `Map[K, V, H]` | row: `values` / `fn values(self) -> impl Iterator[Item = ref V]` | map_keys_values_into; col2 |
| col.120 | `Map[K, V, H]` | row: `values_mut` / `fn values_mut(mut ref self) -> impl Iterator[Item = mut ref V]` / Each value, for in-place change | map_keys_values_into; col2 |
| col.121 | `Map[K, V, H]` | row: `into_keys` / `fn into_keys(own self) -> impl Iterator[Item = K]` / Consumes the map; values drop | map_keys_values_into; col2 |
| col.122 | `Map[K, V, H]` | row: `into_values` / `fn into_values(own self) -> impl Iterator[Item = V]` / Consumes the map; keys drop | map_keys_values_into; col2 |
| col.123 | `Map[K, V, H]` | row: `retain` / `fn retain(mut ref self, pred: own MutFn(K, mut ref V) -> bool)` / Keeps the entries for which `pred` returns `true` and dro | map_retain_changes_values |
| col.124 | `Map[K, V, H]` | row: `entry` / `fn entry(mut ref self, key: own K) -> Entry[K, V]` / The slot for `key`, occupied or vacant | map_entry_or_insert_and_modify |
| col.125 | `Map[K, V, H]` | row: `clear` / `fn clear(mut ref self)` / Drops every entry | map_methods |
| col.126 | `Map[K, V, H]` | row: `reserve` / `fn reserve(mut ref self, additional: i64)` / Makes room for `additional` more entries. `additional <= 0` does nothing. The | map_reserve_and_no_capacity, map_has_no_capacity_refused |
| col.127 | `Map[K, V, H]` | row: `[]` / `m[key]` / Index operator through `Index`. Panics if the key is missing | map_methods, map_index_missing_key_panics |
| col.128 | `Map[K, V, H]` | row: `[]=` / `m[key] = val` / Index assignment through `IndexSet` ([design.md §9](../design.md#index-and-indexmut)): inserts the entry, or r | map_index_set_inserts_and_replaces_dropping_old, map_index_compound_assign_on_missing_key_panics, map_entry_or_insert_and_modify |
| col.129 | `Map[K, V, H]` | **`Entry[K, V]`: insert or modify in place.** `entry` gives one lookup with an in-place update path, with no second hash and no key clone: | map_entry_or_insert_and_modify |
| col.130 | `Map[K, V, H]` | code: enum Entry[K, V] { Occupied { value: mut ref V }, Vacant { key: K, map: mut ref Map[K, V] }, } impl[K: Hash + Eq, V] Entry[K, V] { fn  | map_entry_or_insert_and_modify |
| col.131 | `Map[K, V, H]` | Append to a per-key `Vec` without cloning: | map_entry_or_insert_and_modify |
| col.132 | `Map[K, V, H]` | code: self.table.entry(key).or_insert_with(Vec.new).push(row); | map_entry_or_insert_and_modify |
| col.133 | `Map[K, V, H]` | `or_insert` and `or_insert_with` return a `mut ref V` into the map, which borrows the map ([core-semantics.md §5.4](../core-semantics.md#5-r | map_entry_or_insert_and_modify |
| col.134 | `Map[K, V, H]` | `and_modify` changes an occupied entry and returns the entry for further chaining. | map_entry_or_insert_and_modify |
| col.135 | `Map[K, V, H]` | **Iteration order is unspecified and varies between runs.** The default hasher is seeded per process (see [Hashing](#hashing)), so the order | map_iteration_order_is_unspecified_but_complete, hashing_hash_seed_pins_the_order; arr_fxs |
| col.136 | `Map[K, V, H]` | Code that needs a stable order (sorted output, snapshot tests, reproducible runs) uses `SortedMap`. | map_iteration_order_is_unspecified_but_complete, hashing_hash_seed_pins_the_order; arr_fxs |
| col.137 | `Map[K, V, H]` | `Set` behaves the same way. | map_iteration_order_is_unspecified_but_complete, hashing_hash_seed_pins_the_order; arr_fxs |
| col.138 | `Map[K, V, H]` | **Destruction order follows iteration order.** When a `Map` or `Set` drops, every element drops exactly once, in the unspecified iteration o | map_drop_each_once_key_before_value |
| col.139 | `Map[K, V, H]` | `SortedMap` and `SortedSet` drop in key order. | sorted_map_drops_in_key_order |
| col.140 | `Map[K, V, H]` | See [core-semantics.md §7.8](../core-semantics.md#7-destruction-c6-supersedes-the-drop-judgment-0). | cross-reference, no cell |
| col.141 | `Map[K, V, H]` | A program whose output depends on the order in which `Map` elements run their `Drop` bodies depends on unspecified behaviour. | cross-reference, no cell |
| col.142 | `Set[T, H]` | `Set[T, H = SipHash13BuildHasher]` where `T: Hash + Eq`. | set_methods; col2; arr_hu (a user-hasher Set) |
| col.143 | `Set[T, H]` | row: `new` / `fn new() -> Set[T, H]` / Empty set | set_methods; col2; arr_hu (a user-hasher Set) |
| col.144 | `Set[T, H]` | row: `from` / `fn from(items: own Vec[T]) -> Set[T, H]` / Builds a set from a list; duplicates collapse | set_methods; col2; arr_hu (a user-hasher Set) |
| col.145 | `Set[T, H]` | row: `insert` / `fn insert(mut ref self, val: own T) -> bool` / `false` if already present | set_methods; col2; arr_hu (a user-hasher Set) |
| col.146 | `Set[T, H]` | row: `extend` / `fn extend[I: IntoIterator[Item = T]](mut ref self, items: own I)` / Inserts every item | set_methods; col2; arr_hu (a user-hasher Set) |
| col.147 | `Set[T, H]` | row: `remove` / `fn remove(mut ref self, val: T) -> bool` / `true` if it was present | set_methods; col2; arr_hu (a user-hasher Set) |
| col.148 | `Set[T, H]` | row: `contains` / `fn contains(self, val: T) -> bool` / Membership test | set_methods; col2; arr_hu (a user-hasher Set) |
| col.149 | `Set[T, H]` | row: `len` / `fn len(self) -> i64` / Number of elements | set_methods; col2; arr_hu (a user-hasher Set) |
| col.150 | `Set[T, H]` | row: `is_empty` / `fn is_empty(self) -> bool` / `self.len() == 0` | set_methods; col2; arr_hu (a user-hasher Set) |
| col.151 | `Set[T, H]` | row: `union` / `fn union(self, other: Set[T, H]) -> Set[T, H]` | set_methods; col2; arr_hu (a user-hasher Set) |
| col.152 | `Set[T, H]` | row: `intersection` / `fn intersection(self, other: Set[T, H]) -> Set[T, H]` | set_methods; col2; arr_hu (a user-hasher Set) |
| col.153 | `Set[T, H]` | row: `difference` / `fn difference(self, other: Set[T, H]) -> Set[T, H]` / Elements of `self` not in `other` | set_methods; col2; arr_hu (a user-hasher Set) |
| col.154 | `Set[T, H]` | row: `is_disjoint` / `fn is_disjoint(self, other: Set[T, H]) -> bool` / No element of `self` is in `other` | set_methods; col2; arr_hu (a user-hasher Set) |
| col.155 | `Set[T, H]` | row: `is_subset` / `fn is_subset(self, other: Set[T, H]) -> bool` / Every element of `self` is in `other` | set_methods; col2; arr_hu (a user-hasher Set) |
| col.156 | `Set[T, H]` | row: `is_superset` / `fn is_superset(self, other: Set[T, H]) -> bool` / Every element of `other` is in `self` | set_methods; col2; arr_hu (a user-hasher Set) |
| col.157 | `Set[T, H]` | row: `retain` / `fn retain(mut ref self, pred: own MutFn(T) -> bool)` / Keeps the elements satisfying `pred` and drops the rest | set_methods; col2; arr_hu (a user-hasher Set) |
| col.158 | `Set[T, H]` | row: `reserve` / `fn reserve(mut ref self, additional: i64)` / As `Map.reserve` | set_methods; col2; arr_hu (a user-hasher Set) |
| col.159 | `Set[T, H]` | row: `clear` / `fn clear(mut ref self)` / Drops every element | set_methods; col2; arr_hu (a user-hasher Set) |
| col.160 | `SortedMap[K, V]` | A B-tree map, `K: Ord`. | sorted_map_methods, complexity_large_runs_finish |
| col.161 | `SortedMap[K, V]` | Every operation is O(log n), and iteration is in ascending key order. | sorted_map_methods, complexity_large_runs_finish |
| col.162 | `SortedMap[K, V]` | **Complexity under the MIR interpreter.** The O(log n) bounds for `SortedMap` and `SortedSet` are those of compiled code (`karac build`, and | complexity_large_runs_finish (200,000 inserts in falling order, on the interpreter and compiled) |
| col.163 | `SortedMap[K, V]` | `karac repl`, comptime evaluation and the `--interp` flags run on the MIR interpreter, which keeps a sorted table's entries where they were  | complexity_large_runs_finish (200,000 inserts in falling order, on the interpreter and compiled) |
| col.164 | `SortedMap[K, V]` | The interpreter is the reference for semantics, not for speed ([design.md § Compilation model](../design.md#compilation-model), principle 4) | complexity_large_runs_finish (200,000 inserts in falling order, on the interpreter and compiled) |
| col.165 | `SortedMap[K, V]` | row: `new` / `fn new() -> SortedMap[K, V]` / Empty map | sorted_map_methods; col2 |
| col.166 | `SortedMap[K, V]` | row: `from` / `fn from(pairs: own Vec[(K, V)]) -> SortedMap[K, V]` / Builds a map from key-value pairs; a repeated key keeps its last value | sorted_map_methods; col2 |
| col.167 | `SortedMap[K, V]` | row: `insert` / `fn insert(mut ref self, key: own K, val: own V) -> Option[V]` / Returns the previous value if the key was present | sorted_map_methods; col2 |
| col.168 | `SortedMap[K, V]` | row: `extend` / `fn extend[I: IntoIterator[Item = (K, V)]](mut ref self, items: own I)` / Inserts every pair, in order; a later pair replace | sorted_map_methods; col2 |
| col.169 | `SortedMap[K, V]` | row: `get` / `fn get(self, key: K) -> Option[ref V]` | sorted_map_methods; col2 |
| col.170 | `SortedMap[K, V]` | row: `get_mut` / `fn get_mut(mut ref self, key: K) -> Option[mut ref V]` / Lookup for in-place change, as for `Map` | sorted_map_methods; col2 |
| col.171 | `SortedMap[K, V]` | row: `remove` / `fn remove(mut ref self, key: K) -> Option[V]` | sorted_map_methods; col2 |
| col.172 | `SortedMap[K, V]` | row: `contains_key` / `fn contains_key(self, key: K) -> bool` | sorted_map_methods; col2 |
| col.173 | `SortedMap[K, V]` | row: `entry` / `fn entry(mut ref self, key: own K) -> Entry[K, V]` / The slot for `key`, occupied or vacant, as for `Map` | sorted_map_methods; col2 |
| col.174 | `SortedMap[K, V]` | row: `len` / `fn len(self) -> i64` | sorted_map_methods; col2 |
| col.175 | `SortedMap[K, V]` | row: `is_empty` / `fn is_empty(self) -> bool` | sorted_map_methods; col2 |
| col.176 | `SortedMap[K, V]` | row: `keys` / `fn keys(self) -> impl Iterator[Item = ref K]` / Ascending | sorted_map_methods; col2 |
| col.177 | `SortedMap[K, V]` | row: `values` / `fn values(self) -> impl Iterator[Item = ref V]` / In key order | sorted_map_methods; col2 |
| col.178 | `SortedMap[K, V]` | row: `range` / `fn range(self, from: K, to: K) -> impl Iterator[Item = (ref K, ref V)]` / Entries with keys in `[from, to)`, half-open like  | sorted_map_ranges_and_neighbours; col2 |
| col.179 | `SortedMap[K, V]` | row: `range_inclusive` / `fn range_inclusive(self, from: K, to: K) -> impl Iterator[Item = (ref K, ref V)]` / Entries with keys in `[from, t | sorted_map_ranges_and_neighbours; col2 |
| col.180 | `SortedMap[K, V]` | row: `floor` / `fn floor(self, key: K) -> Option[(ref K, ref V)]` / The entry with the largest key `<= key` | sorted_map_ranges_and_neighbours; col2 |
| col.181 | `SortedMap[K, V]` | row: `ceiling` / `fn ceiling(self, key: K) -> Option[(ref K, ref V)]` / The entry with the smallest key `>= key` | sorted_map_ranges_and_neighbours; col2 |
| col.182 | `SortedMap[K, V]` | row: `lower` / `fn lower(self, key: K) -> Option[(ref K, ref V)]` / The entry with the largest key `< key`. Rust's `range(..k).next_back()` | sorted_map_ranges_and_neighbours; col2 |
| col.183 | `SortedMap[K, V]` | row: `higher` / `fn higher(self, key: K) -> Option[(ref K, ref V)]` / The entry with the smallest key `> key`. Rust's `range((Excluded(k), U | sorted_map_ranges_and_neighbours; col2 |
| col.184 | `SortedMap[K, V]` | row: `first` / `fn first(self) -> Option[(ref K, ref V)]` / The entry with the smallest key. Rust's `first_key_value` | sorted_map_ranges_and_neighbours; col2 |
| col.185 | `SortedMap[K, V]` | row: `last` / `fn last(self) -> Option[(ref K, ref V)]` / The entry with the largest key. Rust's `last_key_value` | sorted_map_ranges_and_neighbours; col2 |
| col.186 | `SortedMap[K, V]` | row: `pop_first` / `fn pop_first(mut ref self) -> Option[(K, V)]` / Removes and returns the entry with the smallest key | sorted_map_pops_and_into |
| col.187 | `SortedMap[K, V]` | row: `pop_last` / `fn pop_last(mut ref self) -> Option[(K, V)]` / Removes and returns the entry with the largest key | sorted_map_pops_and_into |
| col.188 | `SortedMap[K, V]` | row: `values_mut` / `fn values_mut(mut ref self) -> impl Iterator[Item = mut ref V]` / In key order | sorted_map_pops_and_into |
| col.189 | `SortedMap[K, V]` | row: `into_keys` / `fn into_keys(own self) -> impl Iterator[Item = K]` / Ascending; values drop | sorted_map_pops_and_into |
| col.190 | `SortedMap[K, V]` | row: `into_values` / `fn into_values(own self) -> impl Iterator[Item = V]` / In key order; keys drop | sorted_map_pops_and_into |
| col.191 | `SortedMap[K, V]` | row: `retain` / `fn retain(mut ref self, pred: own MutFn(K, mut ref V) -> bool)` / As `Map.retain`, in key order | sorted_map_methods |
| col.192 | `SortedMap[K, V]` | row: `clear` / `fn clear(mut ref self)` | sorted_map_methods |
| col.193 | `SortedMap[K, V]` | row: `[]` / `m[key]` / Index operator through `Index`. Panics if the key is missing | sorted_map_index_missing_key_panics |
| col.194 | `SortedMap[K, V]` | row: `[]=` / `m[key] = val` / Index assignment through `IndexSet`, as for `Map` | sorted_map_methods (`m[9] = ..`) |
| col.195 | `SortedSet[T]` | A B-tree set, `T: Ord`. | sorted_set_methods, complexity_large_runs_finish |
| col.196 | `SortedSet[T]` | Every operation is O(log n), and iteration is in ascending order. | sorted_set_methods, complexity_large_runs_finish |
| col.197 | `SortedSet[T]` | Use `Set[T]` for membership tests alone; use `SortedSet[T]` for min, max, ordered iteration or range queries. | sorted_set_methods, complexity_large_runs_finish |
| col.198 | `SortedSet[T]` | row: `new` / `fn new() -> SortedSet[T]` / Empty set | sorted_set_methods; col2 |
| col.199 | `SortedSet[T]` | row: `from` / `fn from(items: own Vec[T]) -> SortedSet[T]` / Builds a set from a list; duplicates collapse | sorted_set_methods; col2 |
| col.200 | `SortedSet[T]` | row: `insert` / `fn insert(mut ref self, val: own T) -> bool` / `false` if already present | sorted_set_methods; col2 |
| col.201 | `SortedSet[T]` | row: `extend` / `fn extend[I: IntoIterator[Item = T]](mut ref self, items: own I)` / Inserts every item | sorted_set_methods; col2 |
| col.202 | `SortedSet[T]` | row: `remove` / `fn remove(mut ref self, val: T) -> bool` / `true` if it was present | sorted_set_methods; col2 |
| col.203 | `SortedSet[T]` | row: `contains` / `fn contains(self, val: T) -> bool` | sorted_set_methods; col2 |
| col.204 | `SortedSet[T]` | row: `min` / `fn min(self) -> Option[ref T]` / Smallest element | sorted_set_methods; col2 |
| col.205 | `SortedSet[T]` | row: `max` / `fn max(self) -> Option[ref T]` / Largest element | sorted_set_methods; col2 |
| col.206 | `SortedSet[T]` | row: `pop_first` / `fn pop_first(mut ref self) -> Option[T]` / Removes and returns the smallest element | sorted_set_methods; col2 |
| col.207 | `SortedSet[T]` | row: `pop_last` / `fn pop_last(mut ref self) -> Option[T]` / Removes and returns the largest element | sorted_set_methods; col2 |
| col.207a | `SortedSet[T]` | row: `clear` / `fn clear(mut ref self)` / Drops every element in ascending order, as `SortedMap.clear` drops its entries in key order (B-2026-11-26-12) | sorted_set_clear_drops_in_order (drop 1, 2, 3, then the set is empty and reusable) |
| col.208 | `SortedSet[T]` | row: `retain` / `fn retain(mut ref self, pred: own MutFn(T) -> bool)` / Keeps the elements satisfying `pred`, in ascending order | sorted_set_methods; col2 |
| col.209 | `SortedSet[T]` | row: `range` / `fn range(self, from: T, to: T) -> impl Iterator[Item = ref T]` / Elements in `[from, to)`, ascending, half-open like `from.. | sorted_set_methods; col2 |
| col.210 | `SortedSet[T]` | row: `range_inclusive` / `fn range_inclusive(self, from: T, to: T) -> impl Iterator[Item = ref T]` / Elements in `[from, to]`, ascending, li | sorted_set_methods; col2 |
| col.211 | `SortedSet[T]` | row: `len` / `fn len(self) -> i64` | sorted_set_methods; col2 |
| col.212 | `SortedSet[T]` | row: `is_empty` / `fn is_empty(self) -> bool` | sorted_set_methods; col2 |
| col.213 | `SortedSet[T]` | row: `union` / `fn union(self, other: SortedSet[T]) -> SortedSet[T]` | sorted_set_methods; col2 |
| col.214 | `SortedSet[T]` | row: `intersection` / `fn intersection(self, other: SortedSet[T]) -> SortedSet[T]` | sorted_set_methods; col2 |
| col.215 | `SortedSet[T]` | row: `difference` / `fn difference(self, other: SortedSet[T]) -> SortedSet[T]` | sorted_set_methods; col2 |
| col.216 | `SortedSet[T]` | row: `is_disjoint` / `fn is_disjoint(self, other: SortedSet[T]) -> bool` | sorted_set_methods; col2 |
| col.217 | `SortedSet[T]` | row: `is_subset` / `fn is_subset(self, other: SortedSet[T]) -> bool` | sorted_set_methods; col2 |
| col.218 | `SortedSet[T]` | row: `is_superset` / `fn is_superset(self, other: SortedSet[T]) -> bool` | sorted_set_methods; col2 |
| col.219 | Lookup by view | On a `Map`, `Set`, `SortedMap` or `SortedSet` whose key is a `String` or a `Vec[T]`, the lookup methods take the key's view: | lookup_by_view_str_and_slice_keys |
| col.220 | Lookup by view | `Str` for a `String` key, `Slice[T]` for a `Vec[T]` key. | lookup_by_view_str_and_slice_keys |
| col.221 | Lookup by view | The lookup methods are `get`, `get_mut`, `contains`, `contains_key` and `remove`. | lookup_by_view_str_and_slice_keys |
| col.222 | Lookup by view | A lookup with a slice of other text then allocates nothing: | lookup_by_view_str_and_slice_keys |
| col.223 | Lookup by view | code: let n = keywords.get(src[start..end]) ?? 0     // keywords: Map[String, i64] if seen.contains(cand) { ... }                  // seen:  | lookup_by_view_str_and_slice_keys |
| col.224 | Lookup by view | A `String` or `Vec[T]` argument still works, borrowed, because a `String` passes as `Str` and a `Vec[T]` as `Slice[T]` at a call boundary ([ | lookup_by_view_str_and_slice_keys (a String argument; insert and entry take K in every map cell) |
| col.225 | Lookup by view | Every other key type is looked up by `K`, as the tables above show. | lookup_by_view_str_and_slice_keys (a String argument; insert and entry take K in every map cell) |
| col.226 | Lookup by view | Methods that store a key (`insert`, `entry`) still take an owned `K`, and `range`, `floor`, `ceiling`, `lower` and `higher` take `K`. | lookup_by_view_str_and_slice_keys (a String argument; insert and entry take K in every map cell) |
| col.227 | Lookup by view | The same holds for the searches of a sequence of `String` or `Vec[T]` elements: | lookup_by_view_sequence_contains |
| col.228 | Lookup by view | `contains` on a `Slice`, `Vec`, `Array` or `VecDeque`, and `binary_search` where it exists, take the element's view, so `["inc", "dec"].cont | lookup_by_view_sequence_contains |
| col.229 | Lookup by view | The literals of such a list stay `String`s; the argument does not change the element type (review-thread ruling 2026-11-08, B-2026-11-08-55) | lookup_by_view_sequence_contains |
| col.230 | Lookup by view | This works because a view and its owner hash, compare and order identically: | lookup_view_and_owner_hash_alike (FIXED: `s.as_str().hash(mut h)` was refused, B-2026-11-25-28) |
| col.231 | Lookup by view | `s.as_str()` and `s` give the same `hash`, `==` and `cmp` results against any other key, and so do `v.as_slice()` and `v`. | lookup_view_and_owner_hash_alike (FIXED: `s.as_str().hash(mut h)` was refused, B-2026-11-25-28) |
| col.232 | Lookup by view | There is no user-facing trait for this (Rust's `Borrow`). | deferred, no cell |
| col.233 | Lookup by view | A user key type is looked up by the key type itself; see [deferred.md](../deferred.md). | deferred, no cell |
| col.234 | `VecDeque[T]` | A ring buffer with O(1) push and pop at both ends. | vec_deque_methods, complexity_large_runs_finish |
| col.235 | `VecDeque[T]` | row: `new` / `fn new() -> VecDeque[T]` / Empty deque | vec_deque_methods; col3 |
| col.236 | `VecDeque[T]` | row: `with_capacity` / `fn with_capacity(cap: i64) -> VecDeque[T]` / Preallocates `cap` slots. `cap < 0` counts as 0, as for `Vec` | vec_deque_methods; col3 |
| col.237 | `VecDeque[T]` | row: `try_with_capacity` / `fn try_with_capacity(cap: i64) -> Result[VecDeque[T], AllocError]` / As `with_capacity`, with allocation failure | vec_deque_methods; col3 |
| col.238 | `VecDeque[T]` | row: `push_back` / `fn push_back(mut ref self, val: own T)` | vec_deque_methods; col3 |
| col.239 | `VecDeque[T]` | row: `push_front` / `fn push_front(mut ref self, val: own T)` | vec_deque_methods; col3 |
| col.240 | `VecDeque[T]` | row: `pop_back` / `fn pop_back(mut ref self) -> Option[T]` | vec_deque_methods; col3 |
| col.241 | `VecDeque[T]` | row: `pop_front` / `fn pop_front(mut ref self) -> Option[T]` | vec_deque_methods; col3 |
| col.242 | `VecDeque[T]` | row: `front` / `fn front(self) -> Option[ref T]` / The element `pop_front` would return | vec_deque_methods; col3 |
| col.243 | `VecDeque[T]` | row: `back` / `fn back(self) -> Option[ref T]` / The element `pop_back` would return | vec_deque_methods; col3 |
| col.244 | `VecDeque[T]` | row: `get` / `fn get(self, idx: i64) -> Option[ref T]` / Index 0 is the front | vec_deque_methods; col3 |
| col.245 | `VecDeque[T]` | row: `len` / `fn len(self) -> i64` | vec_deque_methods; col3 |
| col.246 | `VecDeque[T]` | row: `is_empty` / `fn is_empty(self) -> bool` | vec_deque_methods; col3 |
| col.247 | `VecDeque[T]` | row: `clear` / `fn clear(mut ref self)` | vec_deque_methods; col3 |
| col.248 | `VecDeque[T]` | row: `contains` / `fn contains(self, val: T) -> bool where T: PartialEq` / Linear search | vec_deque_methods; col3 |
| col.249 | `VecDeque[T]` | row: `insert` / `fn insert(mut ref self, idx: i64, val: own T)` / Inserts at `idx` (0 is the front), shifting toward the nearer end. Panics  | vec_deque_methods, vec_deque_insert_past_len_panics |
| col.250 | `VecDeque[T]` | row: `remove` / `fn remove(mut ref self, idx: i64) -> T` / Removes at `idx`, shifting from the nearer end. Panics if out of bounds | vec_deque_methods, vec_deque_remove_out_of_bounds_panics |
| col.251 | `VecDeque[T]` | row: `swap` / `fn swap(mut ref self, i: i64, j: i64)` / As `Vec.swap` | vec_deque_methods; col3 |
| col.252 | `VecDeque[T]` | row: `retain` / `fn retain(mut ref self, pred: own MutFn(T) -> bool)` / Keeps the elements satisfying `pred`, front to back | vec_deque_methods; col3 |
| col.253 | `VecDeque[T]` | row: `truncate` / `fn truncate(mut ref self, len: i64)` / Drops the elements from index `len` to the back. Does nothing if `len >= self.len( | vec_deque_methods; col3 |
| col.254 | `VecDeque[T]` | row: `extend` / `fn extend[I: IntoIterator[Item = T]](mut ref self, items: own I)` / Pushes every item onto the back, in order | vec_deque_methods; col3 |
| col.255 | `VecDeque[T]` | These are the whole surface, with iteration and the traits below. | vec_deque_methods (the whole table) |
| col.256 | `VecDeque[T]` | A `VecDeque` is not contiguous, so the methods that need one slice (`sort`, `dedup`, `binary_search`, `reverse`, ranges and `as_slice`) are  | vec_deque_sort_refused, vec_deque_as_slice_refused, vec_deque_collected_into_a_vec_sorts |
| col.257 | `PriorityQueue[T]` | A binary heap, `T: Ord`: a `Vec[T]` arranged so that the next element out is at the root. | priority_queue_direction_and_methods, complexity_large_runs_finish (O(n) build of 200,000, then 200,000 pushes and pops) |
| col.258 | `PriorityQueue[T]` | `push` and `pop` are O(log n); `peek`, `len` and `is_empty` are O(1); building from a vector is O(n). | priority_queue_direction_and_methods, complexity_large_runs_finish (O(n) build of 200,000, then 200,000 pushes and pops) |
| col.259 | `PriorityQueue[T]` | **Direction.** `PriorityQueue.new()` pops the smallest element first, as Java's `PriorityQueue` and Python's `heapq` do. | priority_queue_direction_and_methods |
| col.260 | `PriorityQueue[T]` | `PriorityQueue.max_first()` pops the largest first. | priority_queue_direction_and_methods |
| col.261 | `PriorityQueue[T]` | This is the opposite of Rust's `BinaryHeap`. | priority_queue_direction_and_methods |
| col.262 | `PriorityQueue[T]` | The direction is a field of the queue, not a second type, so a queue can be passed to generic code without its direction entering the signat | priority_queue_generic_code_takes_either_direction |
| col.263 | `PriorityQueue[T]` | **Not provided:** `decrease_key` (`push` returns no handle) and a cheap `merge`. | priority_queue_has_no_decrease_key_refused |
| col.264 | `PriorityQueue[T]` | Both need a Fibonacci or pairing heap. | priority_queue_has_no_decrease_key_refused |
| col.265 | `PriorityQueue[T]` | For ordered iteration over the whole collection, use `SortedSet[T]`. | rationale, no cell |
| col.266 | `PriorityQueue[T]` | row: `new` / `fn new() -> PriorityQueue[T]` / Empty, smallest first | priority_queue_direction_and_methods; col3 |
| col.267 | `PriorityQueue[T]` | row: `max_first` / `fn max_first() -> PriorityQueue[T]` / Empty, largest first | priority_queue_direction_and_methods; col3 |
| col.268 | `PriorityQueue[T]` | row: `from` / `fn from(v: own Vec[T]) -> PriorityQueue[T]` / O(n) build, smallest first | priority_queue_direction_and_methods; col3 |
| col.269 | `PriorityQueue[T]` | row: `max_first_from` / `fn max_first_from(v: own Vec[T]) -> PriorityQueue[T]` / O(n) build, largest first | priority_queue_direction_and_methods; col3 |
| col.270 | `PriorityQueue[T]` | row: `push` / `fn push(mut ref self, x: own T)` / O(log n) | priority_queue_direction_and_methods; col3 |
| col.271 | `PriorityQueue[T]` | row: `peek` / `fn peek(self) -> Option[ref T]` / The next element, without removing it | priority_queue_direction_and_methods; col3 |
| col.272 | `PriorityQueue[T]` | row: `pop` / `fn pop(mut ref self) -> Option[T]` / Removes and returns the next element | priority_queue_direction_and_methods; col3 |
| col.273 | `PriorityQueue[T]` | row: `len` / `fn len(self) -> i64` | priority_queue_direction_and_methods; col3 |
| col.274 | `PriorityQueue[T]` | row: `is_empty` / `fn is_empty(self) -> bool` | priority_queue_direction_and_methods; col3 |
| col.275 | `PriorityQueue[T]` | row: `clear` / `fn clear(mut ref self)` / Drops every element | priority_queue_direction_and_methods; col3 |
| col.276 | `PriorityQueue[T]` | row: `into_sorted_vec` / `fn into_sorted_vec(own self) -> Vec[T]` / Consumes the queue in pop order: ascending for smallest-first, descendin | priority_queue_direction_and_methods; col3 |
| col.277 | `PriorityQueue[T]` | row: `iter` / `fn iter(self) -> VecIter[T]` / Every element once, in the queue's own order, not pop order | priority_queue_own_order_is_its_backing_vector |
| col.278 | `PriorityQueue[T]` | row: `into_vec` / `fn into_vec(own self) -> Vec[T]` / The backing vector, in `iter`'s order, unsorted | priority_queue_own_order_is_its_backing_vector |
| col.279 | `PriorityQueue[T]` | The queue's own order, which `iter`, `into_iter`, `for` and `Display` follow, is the order of its backing vector, as with Rust's `BinaryHeap | priority_queue_own_order_is_its_backing_vector |
| col.280 | `PriorityQueue[T]` | It is fixed by the pushes and pops that built the queue, so it is the same on every run and every backend, but it is not pop order; `into_so | priority_queue_own_order_is_its_backing_vector |
| col.281 | `PriorityQueue[T]` | code: let mut pq: PriorityQueue[i64] = PriorityQueue.new(); pq.push(5); pq.push(1); pq.push(4); match pq.peek() { Some(v) => { println(v); } | priority_queue_direction_and_methods (the page's example) |
| col.282 | Iteration | Every collection works with `for`. | iteration_item_types_and_order, iteration_for_borrows_and_into_iter_consumes |
| col.283 | Iteration | A collection is `Iterable`, so `for x in c` borrows `c`; `c.into_iter()` is an `Iterator`, so `for x in c.into_iter()` consumes `c` ([core-s | iteration_item_types_and_order, iteration_for_borrows_and_into_iter_consumes |
| col.284 | Iteration | The iterator methods are: | iteration_item_types_and_order, iteration_for_borrows_and_into_iter_consumes |
| col.285 | Iteration | row: `Vec[T]`, `Array[T, N]`, `Slice[T]` / `ref T` / `mut ref T` / `T` / Index order | iteration_item_types_and_order; map_keys_values_into; priority_queue_own_order_is_its_backing_vector |
| col.286 | Iteration | row: `Map[K, V]` / `(ref K, ref V)` / `(ref K, mut ref V)` / `(K, V)` / Unspecified | iteration_item_types_and_order; map_keys_values_into; priority_queue_own_order_is_its_backing_vector |
| col.287 | Iteration | row: `Set[T]` / `ref T` / none / `T` / Unspecified | iteration_item_types_and_order; map_keys_values_into; priority_queue_own_order_is_its_backing_vector |
| col.288 | Iteration | row: `VecDeque[T]` / `ref T` / `mut ref T` / `T` / Front to back | iteration_item_types_and_order; map_keys_values_into; priority_queue_own_order_is_its_backing_vector |
| col.289 | Iteration | row: `SortedMap[K, V]` / `(ref K, ref V)` / `(ref K, mut ref V)` / `(K, V)` / Ascending key | iteration_item_types_and_order; map_keys_values_into; priority_queue_own_order_is_its_backing_vector |
| col.290 | Iteration | row: `SortedSet[T]` / `ref T` / none / `T` / Ascending | iteration_item_types_and_order; map_keys_values_into; priority_queue_own_order_is_its_backing_vector |
| col.291 | Iteration | row: `PriorityQueue[T]` / `ref T` / none / `T` / The queue's own order, not pop order | iteration_item_types_and_order; map_keys_values_into; priority_queue_own_order_is_its_backing_vector |
| col.292 | Iteration | `Set`, `SortedSet` and `PriorityQueue` have no `iter_mut`, because changing an element in place would break the hash, the ordering or the he | iteration_set_has_no_iter_mut_refused |
| col.293 | Iteration | `iter()` borrows the collection, `iter_mut()` borrows it mutably for the whole iteration, and `into_iter()` consumes it. | iteration_item_types_and_order, iteration_for_borrows_and_into_iter_consumes |
| col.294 | Iteration | Each returns `impl Iterator` with the item shown. | iteration_item_types_and_order, iteration_for_borrows_and_into_iter_consumes |
| col.295 | Iteration | `into_iter` is each collection's `IntoIterator` impl ([design.md § `Iterator` and `Iterable`](../design.md#iterator-and-iterable)), so a gen | iteration_generic_into_iterator_and_extend |
| col.296 | Iteration | Each collection's `extend` is likewise its `Extend` impl (`std.iter.Extend`, [design.md §9](../design.md#9-traits)), so `fn fill[C: Extend[i | iteration_generic_into_iterator_and_extend |
| col.297 | Common traits | `Vec`, `Map`, `Set`, `VecDeque`, `SortedMap`, `SortedSet` and `PriorityQueue` implement `Clone`. | common_traits_clone_display_from_iterator; col3 |
| col.298 | Common traits | Every collection implements `Display` when its elements implement `Display`, and `FromIterator` for its natural element type, so `collect` c | common_traits_clone_display_from_iterator; col3 |
| col.299 | Common traits | `Display` adds only punctuation, never a type name ([design.md §9](../design.md#9-traits)): | common_traits_clone_display_from_iterator; col3 |
| col.300 | Common traits | `Vec`, `Array`, `Slice`, `VecDeque` and `PriorityQueue` (in its own order) render as `[a, b]`, `Map` and `SortedMap` as `{k: v, k2: v2}`, an | common_traits_clone_display_from_iterator; col3 |
| col.301 | Common traits | `Vec[T]`, `Array[T, N]`, `Slice[T]` and `VecDeque[T]` implement `PartialEq` (with `Rhs = Self`) when `T: PartialEq`, and `Eq` when `T: Eq`:  | common_traits_sequence_equality_and_order |
| col.302 | Common traits | They implement `PartialOrd` and `Ord` when `T` does, comparing lexicographically. | common_traits_sequence_equality_and_order |
| col.303 | Common traits | The comparison traits borrow both operands ([design.md §9](../design.md#operator-traits)). | common_traits_sequence_equality_and_order |
| col.304 | Common traits | `Map[K, V]` and `SortedMap[K, V]` implement `PartialEq` when `V: PartialEq`, and `Eq` when `V: Eq`: two maps are equal when they hold the sa | common_traits_map_and_set_equality_ignores_order, common_traits_table_equality_with_a_user_hasher (FIXED: `==` on a table of a user hasher was false compiled, B-2026-11-25-29) |
| col.305 | Common traits | `Set[T]` and `SortedSet[T]` implement `PartialEq` and `Eq`: two sets are equal when they hold the same elements. | common_traits_map_and_set_equality_ignores_order, common_traits_table_equality_with_a_user_hasher (FIXED: `==` on a table of a user hasher was false compiled, B-2026-11-25-29) |
| col.306 | Common traits | Order plays no part, so a `Map` equals a `Map` built by inserting the same entries in another order, and iteration order (random per process | common_traits_map_and_set_equality_ignores_order, common_traits_table_equality_with_a_user_hasher (FIXED: `==` on a table of a user hasher was false compiled, B-2026-11-25-29) |
| col.307 | Common traits | A struct holding one of them can derive `PartialEq`. | common_traits_map_and_set_equality_ignores_order, common_traits_table_equality_with_a_user_hasher (FIXED: `==` on a table of a user hasher was false compiled, B-2026-11-25-29) |
| col.308 | Common traits | They implement no `PartialOrd`; compare `m.iter()` sequences of a `SortedMap` when an order is meant (review-thread ruling 2026-11-08, from  | common_traits_map_has_no_ordering_refused |
| col.309 | `Arena[T]` and `SlotMap[T]` | Two allocation primitives are library types, not language features: | arena_push_get_len_and_rewind |
| col.310 | `Arena[T]` and `SlotMap[T]` | **`Arena[T]`** is a bulk allocator with one lifetime. | arena_push_get_len_and_rewind |
| col.311 | `Arena[T]` and `SlotMap[T]` | Every item is freed together when the arena drops, which suits cache-friendly batch work such as parse trees and per-frame game data. | arena_push_get_len_and_rewind |
| col.312 | `Arena[T]` and `SlotMap[T]` | Items cannot be removed one at a time. | arena_push_get_len_and_rewind |
| col.313 | `Arena[T]` and `SlotMap[T]` | `Arena.new()` makes one; `a.push(x)` stores `x` and returns an `ArenaRef[T]`, `a.get(r)` returns `ref T`, and `a.len()` counts the items. | arena_push_get_len_and_rewind |
| col.314 | `Arena[T]` and `SlotMap[T]` | `a.high_water_mark()` returns an `ArenaCheckpoint`, and `a.rewind_to(cp)` drops everything pushed after it, which makes any `ArenaRef` from  | arena_push_get_len_and_rewind |
| col.315 | `Arena[T]` and `SlotMap[T]` | **`SlotMap[T]`** is a generational-handle allocator with a lifetime per item. | not implemented (unit 318), no cell |
| col.316 | `Arena[T]` and `SlotMap[T]` | Items are inserted and removed one at a time; `slots.insert(x)` returns a `Handle[T]`, and `slots.get(h)` returns `Option[ref T]`, which is  | not implemented (unit 318), no cell |
| col.317 | `Arena[T]` and `SlotMap[T]` | Use it when items have independent lifetimes and stale-handle detection matters (entities, connection tables), and for self-referential data | not implemented (unit 318), no cell |
| col.318 | `Arena[T]` and `SlotMap[T]` | Not yet implemented. | slot_map_is_not_yet_implemented_refused |
| col.319 | `Arena[T]` and `SlotMap[T]` | The name `Pool` belongs to the connection pool, in [pool.md](pool.md). | cross-reference, no cell |
| col.320 | Fallible allocation | An allocation can fail at run time: the kernel refuses memory, a fixed heap region is full, or a requested capacity is too large to represen | fallible_twins_return_results, fallible_twins_of_tables |
| col.321 | Fallible allocation | Every collection method that allocates comes in two forms: the plain method, which panics on allocation failure, and a `try_*` twin that ret | fallible_twins_return_results, fallible_twins_of_tables |
| col.322 | Fallible allocation | **Default behaviour.** Every allocating method (`Vec.push`, `Vec.insert`, `Vec.extend`, `Vec.reserve`, `Vec.with_capacity`, `String.push`, ` | effects_alloc_and_panics_are_default_permitted (allocates, no separate panics atom: conformance/design/effects allocation_alone_is_not_a_panics_atom) |
| col.323 | Fallible allocation | Most code cannot do anything useful when memory runs out, so this is the default. | effects_alloc_and_panics_are_default_permitted (allocates, no separate panics atom: conformance/design/effects allocation_alone_is_not_a_panics_atom) |
| col.324 | Fallible allocation | That panic is covered by the method's `allocates(Heap)` effect, not by a separate `panics` ([design.md §12](../design.md#default-permitted-e | effects_alloc_and_panics_are_default_permitted (allocates, no separate panics atom: conformance/design/effects allocation_alone_is_not_a_panics_atom) |
| col.325 | Fallible allocation | **Fallible twins.** Each panicking method has a `try_*` companion. | fallible_twins_return_results, fallible_twins_of_tables, fallible_capacity_overflow_is_an_err |
| col.326 | Fallible allocation | Operations that report only success or failure return `Result[(), AllocError]` (`try_push`, `try_insert` on a `Vec` or `VecDeque`, `try_rese | fallible_twins_return_results, fallible_twins_of_tables, fallible_capacity_overflow_is_an_err |
| col.327 | Fallible allocation | A twin whose panicking method returns a value returns that value in the `Ok`: | fallible_twins_return_results, fallible_twins_of_tables, fallible_capacity_overflow_is_an_err |
| col.328 | Fallible allocation | `Map.try_insert(k, v)` is `Result[Option[V], AllocError]`, the displaced value as `insert` returns it, and `Set.try_insert(x)` is `Result[bo | fallible_twins_return_results, fallible_twins_of_tables, fallible_capacity_overflow_is_an_err |
| col.329 | Fallible allocation | Constructors and capacity-bearing operations return `Result[T, AllocError]` (`try_with_capacity`, `try_new`). | fallible_twins_return_results, fallible_twins_of_tables, fallible_capacity_overflow_is_an_err |
| col.330 | Fallible allocation | code: // Panics on allocation failure let mut v: Vec[i64] = Vec.new(); v.push(42); // Returns the failure as a value let mut w: Vec[i64] = V | fallible_twins_return_results, fallible_twins_of_tables, fallible_capacity_overflow_is_an_err |
| col.331 | Fallible allocation | The two forms are independent, and the fallible one is always available. | fallible_twins_return_results, fallible_twins_of_tables, fallible_capacity_overflow_is_an_err |
| col.332 | Fallible allocation | A caller that wants explicit handling at one site calls `try_push(x)?` there. | fallible_twins_return_results, fallible_twins_of_tables, fallible_capacity_overflow_is_an_err |
| col.333 | Fallible allocation | **`AllocError`** is a prelude type: | fallible_capacity_overflow_is_an_err, alloc_error_traits_and_display |
| col.334 | Fallible allocation | code: enum AllocError { OutOfMemory { requested_bytes: i64 }, CapacityOverflow, } | fallible_capacity_overflow_is_an_err, alloc_error_traits_and_display |
| col.335 | Fallible allocation | `OutOfMemory` carries the byte count the allocator could not satisfy, for diagnostics, retrying with a smaller request, or telemetry. | fallible_capacity_overflow_is_an_err (a real out-of-memory cannot be provoked in a cell) |
| col.336 | Fallible allocation | `CapacityOverflow` means the requested capacity could not be represented, so the allocator was never asked; it is a logic error that code sh | fallible_capacity_overflow_is_an_err (a real out-of-memory cannot be provoked in a cell) |
| col.337 | Fallible allocation | `AllocError` implements `Debug`, `Display`, `Eq` and `Copy`. | alloc_error_traits_and_display, fallible_twins_return_results |
| col.338 | Fallible allocation | Its `Display` names the byte count for `OutOfMemory` and prints "capacity overflow" for `CapacityOverflow`. | alloc_error_traits_and_display, fallible_twins_return_results |
| col.339 | Fallible allocation | A `Result[T, AllocError]` propagates through `?` like any other `Result`; a function that calls `try_*` methods converts `AllocError` into i | alloc_error_traits_and_display, fallible_twins_return_results |
| col.340 | Fallible allocation | **Methods and their twins:** | heading |
| col.341 | Fallible allocation | row: `Vec[T]` / `new()` / `new()` (does not allocate) | fallible_twins_return_results, fallible_twins_of_tables |
| col.342 | Fallible allocation | row: `with_capacity(n)` / `try_with_capacity(n)` | fallible_twins_return_results, fallible_twins_of_tables |
| col.343 | Fallible allocation | row: `push(x)` / `try_push(x)` | fallible_twins_return_results, fallible_twins_of_tables |
| col.344 | Fallible allocation | row: `insert(i, x)` / `try_insert(i, x)` | fallible_twins_return_results, fallible_twins_of_tables |
| col.345 | Fallible allocation | row: `extend(items)` / `try_extend(items)` | fallible_twins_return_results, fallible_twins_of_tables |
| col.346 | Fallible allocation | row: `extend_from_slice(items)` / `try_extend_from_slice(items)` | fallible_twins_return_results, fallible_twins_of_tables |
| col.347 | Fallible allocation | row: `reserve(n)` / `try_reserve(n)` | fallible_twins_return_results, fallible_twins_of_tables |
| col.348 | Fallible allocation | row: `reserve_exact(n)` / `try_reserve_exact(n)` | fallible_twins_return_results, fallible_twins_of_tables |
| col.349 | Fallible allocation | row: `resize(n, val)` / `try_resize(n, val)` | fallible_twins_return_results, fallible_twins_of_tables |
| col.350 | Fallible allocation | row: `clone()` / `try_clone()` | fallible_twins_return_results, fallible_twins_of_tables |
| col.351 | Fallible allocation | row: `collect()` into a `Vec` / `Vec.try_from_iter(iter)` | fallible_twins_return_results, fallible_twins_of_tables |
| col.352 | Fallible allocation | row: `String` / `new()` / `new()` (does not allocate) | fallible_twins_return_results, fallible_twins_of_tables |
| col.353 | Fallible allocation | row: `push(c)` / `try_push(c)` | fallible_twins_return_results, fallible_twins_of_tables |
| col.354 | Fallible allocation | row: `push_str(s)` / `try_push_str(s)` | fallible_twins_return_results, fallible_twins_of_tables |
| col.355 | Fallible allocation | row: `with_capacity(n)` / `try_with_capacity(n)` | fallible_twins_return_results, fallible_twins_of_tables |
| col.356 | Fallible allocation | row: `reserve(n)` / `try_reserve(n)` | fallible_twins_return_results, fallible_twins_of_tables |
| col.357 | Fallible allocation | row: `clone()` / `try_clone()` | fallible_twins_return_results, fallible_twins_of_tables |
| col.358 | Fallible allocation | row: `Map[K, V]`, `Set[T]`, `VecDeque[T]`, `SortedSet[T]` / `new()` / `new()` (does not allocate) | fallible_twins_return_results, fallible_twins_of_tables |
| col.359 | Fallible allocation | row: `with_capacity(n)` (where present) / `try_with_capacity(n)` | fallible_twins_return_results, fallible_twins_of_tables |
| col.360 | Fallible allocation | row: `insert(...)` / `try_insert(...)` | fallible_twins_return_results, fallible_twins_of_tables |
| col.361 | Fallible allocation | row: `reserve(n)` (where present) / `try_reserve(n)` | fallible_twins_return_results, fallible_twins_of_tables |
| col.362 | Fallible allocation | row: `clone()` / `try_clone()` | fallible_twins_return_results, fallible_twins_of_tables |
| col.363 | Fallible allocation | **Why two methods, not one return type that depends on configuration.** A return type that changed with project configuration (`fn push(...) | fallible_generic_fill_calls_try_push |
| col.364 | Fallible allocation | With two methods, the caller picks the form it needs and generic code calls that form: | fallible_generic_fill_calls_try_push |
| col.365 | Fallible allocation | `fn fill[T: Clone](v: mut ref Vec[T], n: i64, val: T) -> Result[(), AllocError]` calls `try_push` and compiles the same everywhere. | fallible_generic_fill_calls_try_push |
| col.366 | Fallible allocation | A project-wide switch that rejects the panicking forms is part of the systems track ([deferred.md](../deferred.md#systems)). | deferred, no cell |
| col.367 | Hashing | To key a table by handle identity rather than by value, use `Identity[T]` (`Map[Identity[Node], Node]`, `Set[Identity[Node]]`): see [design. | hashing_identity_keys |
| col.368 | Hashing | `Map` and `Set` hash their keys through two traits. | hashing_user_build_hasher; col4 |
| col.369 | Hashing | `Hash` (a type's way of feeding itself to a hasher) and `derive(Hash)` are in [design.md §9](../design.md#9-traits), together with the rule  | hashing_user_build_hasher; col4 |
| col.370 | Hashing | The other half, in `std.hash`, is the hasher: | hashing_user_build_hasher; col4 |
| col.371 | Hashing | code: trait Hasher { fn write(mut ref self, bytes: Slice[u8]); fn finish(self) -> u64; // Provided methods that feed fixed-width integers th | hashing_user_build_hasher; col4 |
| col.372 | Hashing | The split lets the hash algorithm be a property of the table, not of the key: | hashing_builders_build_hashers_that_match_the_table |
| col.373 | Hashing | `Map[K, V, FxBuildHasher]` and `Map[K, V, SipHash13BuildHasher]` use the same `Hash` impls for `K`. | hashing_builders_build_hashers_that_match_the_table |
| col.374 | Hashing | A `BuildHasher` makes a fresh `Hasher` for each hash, so per-hash state (the `Hasher`) stays separate from per-table configuration (the buil | hashing_builders_build_hashers_that_match_the_table |
| col.375 | Hashing | **Hashers the library provides.** | heading |
| col.376 | Hashing | `SipHash13BuildHasher` is the default for `Map[K, V]` and `Set[T]`. | hashing_hash_seed_pins_the_order (the random key and the embedded fixed key are not observable in a cell) |
| col.377 | Hashing | It is resistant to hash flooding and keyed from a random value chosen once per process: the operating system's random source (`/dev/urandom` | hashing_hash_seed_pins_the_order (the random key and the embedded fixed key are not observable in a cell) |
| col.378 | Hashing | An embedded build, which has none, uses a fixed key. | hashing_hash_seed_pins_the_order (the random key and the embedded fixed key are not observable in a cell) |
| col.379 | Hashing | The default may change in a future Kāra version if a better algorithm of the same kind becomes standard. | hashing_hash_seed_pins_the_order (the random key and the embedded fixed key are not observable in a cell) |
| col.380 | Hashing | `FxBuildHasher` is fast and unkeyed. | hashing_builders_build_hashers_that_match_the_table; arr_fxo, arr_fx, arr_h (Fx tables) |
| col.381 | Hashing | A table that names it gives up the protection against hash flooding. | hashing_builders_build_hashers_that_match_the_table; arr_fxo, arr_fx, arr_h (Fx tables) |
| col.382 | Hashing | Both are ordinary `BuildHasher` types: | hashing_builders_build_hashers_that_match_the_table; corpus/design/ok_library_build_hashers_build_and_meet_build_hasher |
| col.383 | Hashing | `build()` makes a `SipHash13Hasher` or an `FxHasher` (`std.hash`), whose `finish` digests the bytes written with the same function a table o | hashing_builders_build_hashers_that_match_the_table; corpus/design/ok_library_build_hashers_build_and_meet_build_hasher |
| col.384 | Hashing | A `Map` or `Set` that names one still hashes through a native fast path inside the table rather than through these types; the digests are th | hashing_builders_build_hashers_that_match_the_table; corpus/design/ok_library_build_hashers_build_and_meet_build_hasher |
| col.385 | Hashing | **A user hasher** is any type that implements `BuildHasher`, named in the table's last type parameter: | hashing_user_build_hasher; arr_hu, arr_hf, arr_hf2, arr_hf3, arr_hc, arr_dc, arr_t1, arr_t2, arr_t3, arr_t4 |
| col.386 | Hashing | `Map[K, V, MyBuildHasher]`, `Set[T, MyBuildHasher]`. | hashing_user_build_hasher; arr_hu, arr_hf, arr_hf2, arr_hf3, arr_hc, arr_dc, arr_t1, arr_t2, arr_t3, arr_t4 |
| col.387 | Hashing | The builder must be a struct with no fields, because the table names its builder as a type and constructs it itself; a seed belongs inside ` | hashing_user_build_hasher_with_fields_refused |
| col.388 | Hashing | A field-carrying builder is a compile error. | hashing_user_build_hasher_with_fields_refused |
| col.389 | Hashing | **Keys use their own impls.** A table hashes each key through the key type's `Hash` impl and compares keys through its `PartialEq` impl (whi | hashing_keys_use_their_own_impls |
| col.390 | Hashing | So an impl that deliberately ignores a field puts two keys that differ only in that field in the same slot. | hashing_keys_use_their_own_impls |
| col.391 | Hashing | `Hash` decides which bytes a key contributes and the table's `BuildHasher` decides how those bytes become a digest, so a hand-written `Hash` | hashing_keys_use_their_own_impls |
| col.392 | Hashing | **What the built-in types feed.** The library's `Hash` impls feed these bytes, so a user hasher sees the same input on every backend: | col4 (every feed in the list, against the page's table); corpus/core/ok_hash_into_user_hasher |
| col.393 | Hashing | an integer: its own width through `write_<width>` (`write_i64` for `i64`, `write_u8` for `u8`), native-endian; | col4 (every feed in the list, against the page's table); corpus/core/ok_hash_into_user_hasher |
| col.394 | Hashing | `bool`: | col4 (every feed in the list, against the page's table); corpus/core/ok_hash_into_user_hasher |
| col.395 | Hashing | `write_u8` of 0 or 1; `char`: | col4 (every feed in the list, against the page's table); corpus/core/ok_hash_into_user_hasher |
| col.396 | Hashing | `write_u32` of its scalar value; | col4 (every feed in the list, against the page's table); corpus/core/ok_hash_into_user_hasher |
| col.397 | Hashing | `F32` and `F64` ([design.md § Floats](../design.md)): | col4 (every feed in the list, against the page's table); corpus/core/ok_hash_into_user_hasher |
| col.398 | Hashing | `write_u32` / `write_u64` of the bits, because the wrappers compare by bits, so `-0.0` and `0.0` are different keys and NaN (canonicalized o | hashing_f64_wrapper_keys_compare_by_bits; col4 |
| col.399 | Hashing | The primitives `f32` and `f64` have no `Hash`, so `Map[f64, V]` is refused; wrap the key in `F64`; | hashing_f64_key_refused; tests/collections_conformance.rs coll_float_key_diagnostic_names_the_total_order_float |
| col.400 | Hashing | `String` and `Str`: | col4 (every feed in the list, against the page's table); corpus/core/ok_hash_into_user_hasher |
| col.401 | Hashing | `write` of the UTF-8 bytes, then `write_u8(0xff)`, so `("ab", "c")` and `("a", "bc")` feed different bytes; | col4 (every feed in the list, against the page's table); corpus/core/ok_hash_into_user_hasher |
| col.402 | Hashing | `Vec`, `Array` and `Slice`: | col4 (every feed in the list, against the page's table); corpus/core/ok_hash_into_user_hasher |
| col.403 | Hashing | `write_usize` of the length, then each element; | col4 (every feed in the list, against the page's table); corpus/core/ok_hash_into_user_hasher |
| col.404 | Hashing | a tuple, and a `derive(Hash)` struct: each field in order; a `derive(Hash)` enum: | col4 (every feed in the list, against the page's table); corpus/core/ok_hash_into_user_hasher |
| col.405 | Hashing | `write_u32` of the variant's index, then its payload's fields in order; | col4 (every feed in the list, against the page's table); corpus/core/ok_hash_into_user_hasher |
| col.406 | Hashing | `Option[T]` and `Result[T, E]` as the enums they are. | col4 (every feed in the list, against the page's table); corpus/core/ok_hash_into_user_hasher |
| col.407 | Hashing | **Stability policy.** The default hasher's values are not stable across runs, across Kāra versions or across targets. | map_iteration_order_is_unspecified_but_complete; arr_fxs |
| col.408 | Hashing | This matches Python and modern Java: the common use is in-memory keying, and the common attack is hash flooding from adversarial input. | map_iteration_order_is_unspecified_but_complete; arr_fxs |
| col.409 | Hashing | Iteration order changes with them (see [`Map`](#mapk-v-h)). | map_iteration_order_is_unspecified_but_complete; arr_fxs |
| col.410 | Hashing | For testing, the environment variable `KARAC_HASH_SEED` (a decimal or `0x`-hexadecimal `u64`) pins the per-process key so that a run can be  | hashing_hash_seed_pins_the_order, hashing_hash_seed_hex_is_the_same_pin, hashing_hash_seed_malformed_warns_on_stderr (wasm: tests/mir_auto_par_wasm.rs, B-2026-11-17-64) |
| col.411 | Hashing | It is a testing aid only: a deployment must not set it, since a known key gives up the protection the default exists for. | guidance, no cell |
| col.412 | Hashing | **Stable digests use `StableHash`, not `Hash`.** Content addressing, on-disk indexes, snapshot tests and sharding need a digest that never c | stable_hash_siphash24 |
| col.413 | Hashing | `StableHash` provides explicit, versioned functions for this. | stable_hash_siphash24 |
| col.414 | Hashing | They take bytes, name the algorithm in the function name, take the key as an argument, and are versioned separately from the `Hash` trait. | stable_hash_siphash24 |
| col.415 | Hashing | `karac explain --concept=stable-hash` points users who reach for `Hash` here. | tests/cli.rs (`karac explain --concept=stable-hash`) |
| col.416 | Hashing | code: fn content_id(payload: Str) -> u64 { StableHash.siphash24(payload.bytes(), 0, 0) } | stable_hash_siphash24 (the reference vector and the 15-byte keyed vector); col4 |
| col.417 | Hashing | `StableHash.siphash24(bytes: Slice[u8], k0: u64, k1: u64) -> u64` computes SipHash-2-4 over `bytes` under the caller's 128-bit key and reads | stable_hash_siphash24 (the reference vector and the 15-byte keyed vector); col4 |
| col.418 | Hashing | The same bytes under the same key give the same value across runs, versions, targets and machines. | stable_hash_siphash24 (the reference vector and the 15-byte keyed vector); col4 |
| col.419 | Hashing | It uses 2-4 rounds, the count the SipHash paper specifies, so that it agrees with other languages' `siphash24`. | stable_hash_siphash24 (the reference vector and the 15-byte keyed vector); col4 |
| col.420 | Hashing | The key is required because changing it changes every stored digest, and that belongs at the call site. | stable_hash_siphash24 (the reference vector and the 15-byte keyed vector); col4 |
| col.421 | Hashing | `StableHash` is a namespace that is never constructed. | stable_hash_siphash24 (the reference vector and the 15-byte keyed vector); col4 |
| col.422 | Hashing | **`StableHash` is not a cryptographic hash.** SipHash is a fast keyed function, not collision-resistant, and unfit for signatures or for ded | rationale, no cell (std.crypto: conformance/crypto) |
| col.423 | Hashing | Cryptographic hashing belongs to `std.crypto`: | rationale, no cell (std.crypto: conformance/crypto) |
| col.424 | Hashing | SHA-256 and the other interoperability digests are in `std.crypto.sha2` ([crypto.md](crypto.md)). | rationale, no cell (std.crypto: conformance/crypto) |
| col.425 | Hashing | Its general hash for new designs is BLAKE3, which has not shipped yet ([deferred.md](../deferred.md)). | rationale, no cell (std.crypto: conformance/crypto) |
| col.426 | Hashing | **There is no `xxh3`.** A fast digest for content addressing is better served by BLAKE3, which is in the same speed class and also collision | hashing_no_xxh3_refused |
| col.427 | Hashing | A third stable-hash option that resists neither flooding nor collisions would add a choice every user must get right, for no new capability. | hashing_no_xxh3_refused |
