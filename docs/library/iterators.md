# Iterators

The lang-item traits `Iterator` and `Iterable`, and the way `for` uses them, are in [design.md §9](../design.md#9-traits) and [core-semantics.md §4.6](../core-semantics.md#4-parameters-calls-and-patterns): `for x in e` consumes `e` when its type is an `Iterator`, and otherwise borrows it through `Iterable`. `IntoIterator`, whose `into_iter(own self)` turns a value into an iterator, is an ordinary library trait. An `Iterator` implements one method, `next(mut ref self) -> Option[Self.Item]`. This file specifies the methods every iterator gets on top of it.

## How the methods work

- **They are provided methods of `Iterator`, written in Kāra.** A type that implements `next` gets every method below, whether it is a library iterator (`v.iter()`, `s.chars()`, a range) or a user type.
- **An adaptor returns an adapter struct.** `it.map(f)` returns a struct in `std.iter` that holds `it` and `f` and implements `Iterator`. The struct's name is not part of the contract, so the signatures below write the result as `impl Iterator[Item = U]`.
- **An adaptor takes its closure as a generic parameter** (`f: own F` with `F: MutFn(own Self.Item) -> U`), not as a function-typed parameter, because the adapter stores it. Storing it there does not make it escape: the adapter is a local view ([core-semantics.md §9.3](../core-semantics.md#9-closures)).
- **The adapter is a view when what it holds is one.** A closure that captures by `ref` or `mut ref` is a view ([core-semantics.md §9.2](../core-semantics.md#9-closures)), and so is an iterator over a borrowed collection. An adapter holding either is a view too, and [core-semantics.md §5](../core-semantics.md#5-references-and-views-c5) governs it: it can be used and passed down, but not stored where §5.7 forbids. So `xs.iter().filter(|r| r.x > limit)` works with no extra rule. An adapter over owned values whose closure borrows nothing is an ordinary value, and may be returned or stored.
- **Adaptors are lazy.** An adaptor computes nothing when called. Items are produced one at a time when a terminal method (`fold`, `collect`, `count`, `any`, `all`, `find` and the rest) or a `for` loop pulls them, so `xs.iter().map(f).filter(g).collect()` builds no intermediate collection.
- **Fusion is an optimization.** The compiler may turn a chain into a single loop. That never changes what the program does.
- **Effects flow through a chain.** A source's `next` may have effects: the iterator from `stdin.lines()` reads `Stdin` and blocks. Each adaptor adds the effects of the function passed to it. A chain's effects are the source's plus every function's, counted once for the loop that drives it ([design.md §12](../design.md#12-effects)). One `Iterator` trait therefore covers pure and blocking sources alike, with no separate stream type.

## Adaptors

| Method | Signature | Notes |
|---|---|---|
| `map` | `fn map[U, F: MutFn(own Self.Item) -> U](own self, f: own F) -> impl Iterator[Item = U]` | |
| `filter` | `fn filter[F: MutFn(Self.Item) -> bool](own self, pred: own F) -> impl Iterator[Item = Self.Item]` | Items that pass are yielded by value; items that fail are dropped |
| `filter_map` | `fn filter_map[U, F: MutFn(own Self.Item) -> Option[U]](own self, f: own F) -> impl Iterator[Item = U]` | Yields the `Some` results |
| `flat_map` | `fn flat_map[U: Iterator, F: MutFn(own Self.Item) -> U](own self, f: own F) -> impl Iterator[Item = U.Item]` | |
| `flatten` | `fn flatten(own self) -> impl Iterator[Item = Self.Item.Item] where Self.Item: Iterator` | |
| `enumerate` | `fn enumerate(own self) -> impl Iterator[Item = (i64, Self.Item)]` | Pairs each item with its index, from 0 |
| `zip` | `fn zip[Other: Iterator](own self, other: own Other) -> impl Iterator[Item = (Self.Item, Other.Item)]` | Ends when either side ends |
| `chain` | `fn chain[Other: Iterator[Item = Self.Item]](own self, other: own Other) -> impl Iterator[Item = Self.Item]` | All of `self`, then all of `other` |
| `take` | `fn take(own self, n: i64) -> impl Iterator[Item = Self.Item]` | At most `n` items |
| `skip` | `fn skip(own self, n: i64) -> impl Iterator[Item = Self.Item]` | Drops the first `n` items |
| `take_while` | `fn take_while[F: MutFn(Self.Item) -> bool](own self, pred: own F) -> impl Iterator[Item = Self.Item]` | Stops at the first item that fails |
| `skip_while` | `fn skip_while[F: MutFn(Self.Item) -> bool](own self, pred: own F) -> impl Iterator[Item = Self.Item]` | Skips items while `pred` holds, then yields the rest |
| `step_by` | `fn step_by(own self, step: i64) -> impl Iterator[Item = Self.Item]` | The first item, then every `step`-th |
| `rev` | `fn rev(own self) -> impl Iterator[Item = Self.Item] where Self: DoubleEndedIterator` | Back to front |
| `peekable` | `fn peekable(own self) -> Peekable[Self]` | Adds `peek(mut ref self) -> Option[ref Self.Item]`, which looks at the next item without consuming it |
| `inspect` | `fn inspect[F: MutFn(Self.Item)](own self, f: own F) -> impl Iterator[Item = Self.Item]` | Calls `f` on each item as it passes |
| `scan` | `fn scan[A, U, F: MutFn(own A, own Self.Item) -> Option[(A, U)]](own self, init: own A, f: own F) -> impl Iterator[Item = U]` | Threads a state: `f` returns the next state and the item to yield. Stops at the first `None` |
| `by_ref` | `fn by_ref(mut ref self) -> impl Iterator[Item = Self.Item]` | Borrows the iterator instead of consuming it; see below |

`filter`, `take_while`, `skip_while`, `find`, `position` and `partition` lend each item to the predicate (a bare `Self.Item`), so the test does not consume it; only items that pass are yielded by value.

**Looping over part of an iterator.** `for x in it` consumes `it`. To stop part-way and keep the rest, loop over `it.by_ref()`, which borrows `it` mutably for the loop; afterwards `it` holds the items not yet taken:

```kara
let mut words = line.split_whitespace();
let mut flags: Vec[Str] = Vec.new();
for w in words.by_ref() {
    if w == "--" { break; }
    flags.push(w);
}
let rest: Vec[Str] = words.collect();   // the words after "--"
```

## Terminal methods

| Method | Signature | Notes |
|---|---|---|
| `fold` | `fn fold[A](own self, init: own A, f: own MutFn(own A, own Self.Item) -> A) -> A` | |
| `reduce` | `fn reduce(own self, f: own MutFn(own Self.Item, own Self.Item) -> Self.Item) -> Option[Self.Item]` | `None` if empty |
| `count` | `fn count(own self) -> i64` | Number of items |
| `sum` | `fn sum(own self) -> Self.Item` | Numeric items; 0 if empty. Overflow follows the item type's arithmetic |
| `product` | `fn product(own self) -> Self.Item` | Numeric items; 1 if empty |
| `min` | `fn min(own self) -> Option[Self.Item] where Self.Item: Ord` | `None` if empty |
| `max` | `fn max(own self) -> Option[Self.Item] where Self.Item: Ord` | `None` if empty |
| `min_by` | `fn min_by(own self, cmp: own MutFn(Self.Item, Self.Item) -> Ordering) -> Option[Self.Item]` | |
| `max_by` | `fn max_by(own self, cmp: own MutFn(Self.Item, Self.Item) -> Ordering) -> Option[Self.Item]` | |
| `min_by_key` | `fn min_by_key[K: Ord](own self, key: own MutFn(Self.Item) -> K) -> Option[Self.Item]` | |
| `max_by_key` | `fn max_by_key[K: Ord](own self, key: own MutFn(Self.Item) -> K) -> Option[Self.Item]` | |
| `any` | `fn any(own self, pred: own MutFn(own Self.Item) -> bool) -> bool` | Stops at the first `true` |
| `all` | `fn all(own self, pred: own MutFn(own Self.Item) -> bool) -> bool` | Stops at the first `false` |
| `find` | `fn find(own self, pred: own MutFn(Self.Item) -> bool) -> Option[Self.Item]` | First item that passes |
| `find_map` | `fn find_map[U](own self, f: own MutFn(own Self.Item) -> Option[U]) -> Option[U]` | First `Some` result |
| `position` | `fn position(own self, pred: own MutFn(Self.Item) -> bool) -> Option[i64]` | Index of the first item that passes |
| `last` | `fn last(own self) -> Option[Self.Item]` | |
| `nth` | `fn nth(own self, n: i64) -> Option[Self.Item]` | The item at index `n` |
| `collect` | `fn collect[C: FromIterator[Self.Item]](own self) -> C` | See below |
| `for_each` | `fn for_each(own self, f: own MutFn(own Self.Item))` | |
| `partition` | `fn partition(own self, pred: own MutFn(Self.Item) -> bool) -> (Vec[Self.Item], Vec[Self.Item])` | Items that pass, then items that fail |

A terminal method calls its function before it returns and keeps nothing, so it takes an ordinary non-escaping function parameter. The number of items is `count()`; iterators have no `len`.

## `collect` and `FromIterator`

`collect` builds a collection through a conversion trait:

```kara
trait FromIterator[T] {
    fn from_iter[I: Iterator[Item = T]](iter: own I) -> Self;
}
```

`Vec`, `Map`, `Set`, `VecDeque`, `SortedMap`, `SortedSet` and `String` implement `FromIterator` for their natural element type (`(K, V)` for the maps, `char` for `String`). `collect` takes its target type from context, so it usually needs an annotation:

```kara
let v: Vec[i32] = it.collect();
let chars: Vec[char] = s.chars().collect();
```

## `DoubleEndedIterator`

An iterator that can also yield from the back implements `DoubleEndedIterator` (in `std.iter`):

```kara
trait DoubleEndedIterator: Iterator {
    fn next_back(mut ref self) -> Option[Self.Item];
}
```

The iterators over `Vec`, `Array`, `Slice` and `VecDeque`, and integer ranges, implement it, which is what `rev` needs.

## Example

```kara
fn load_all(paths: Slice[String]) -> Vec[Result[String, IoError]] {
    paths.iter()
        .filter(|p| p.ends_with(".txt"))      // no effects
        .map(|p| fs.read_to_string(p))        // reads(FileSystem), blocks
        .collect()
}
```

Nothing is read until `collect` pulls the first item. The chain, and so `load_all`, has `reads(FileSystem)` and `blocks`.
