# Getting Started, Part 2: Two Surfaces

Kāra is one language with two everyday surfaces. You can save your code in a `.kara` file and run it with `karac run`, or you can paste it line by line into `karac repl` and watch each piece take effect immediately. Both surfaces run the same compiler, see the same diagnostics, and apply the same ownership rules. The difference is the rhythm: a file is a finished thought, the REPL is a thought in progress.

This chapter walks one example — a binary search over a sorted vector — through both surfaces side by side, so you can feel where each one shines.

## The same program, on disk

Save this to `search.kara`:

```kara,run
fn binary_search(haystack: Vec[i32], needle: i32) -> Option[i64] {
    let mut lo: i64 = 0;
    let mut hi: i64 = haystack.len();
    while lo < hi {
        let mid = (lo + hi) / 2;
        let value = haystack[mid];
        if value == needle {
            return Some(mid);
        } else if value < needle {
            lo = mid + 1;
        } else {
            hi = mid;
        }
    }
    None
}

fn main() {
    let nums: Vec[i32] = [1, 3, 5, 7, 9, 11, 13];
    match binary_search(nums, 7) {
        Some(i) => println(f"found at index {i}"),
        None => println("not found"),
    }
}
```

Then run it:

```text
$ karac run search.kara
found at index 3
```

A few things worth pointing at:

- **`haystack: Vec[i32]`** borrows: a bare parameter reads the caller's value without taking ownership of it. The caller keeps `nums` and can use it again afterward. A function that takes ownership writes `own Vec[i32]`.
- **`Option[i64]`** is the standard "maybe an index" return. Pattern-match on it; the compiler will warn you if you forget a case.
- **No allocator imports, no module declarations.** A `.kara` file with `fn main()` is a complete program.

## The same program, in the REPL

Now start the REPL:

```text
$ karac repl
Kāra REPL  (type :help for commands, :quit to exit)
> 
```

We'll build the same example cell by cell. Each line you submit is a *cell* — its own unit of evaluation, kept around so later cells can see it.

```text
> fn binary_search(haystack: Vec[i32], needle: i32) -> Option[i64] {
..     let mut lo: i64 = 0;
..     let mut hi: i64 = haystack.len();
..     while lo < hi {
..         let mid = (lo + hi) / 2;
..         let value = haystack[mid];
..         if value == needle { return Some(mid); }
..         else if value < needle { lo = mid + 1; }
..         else { hi = mid; }
..     }
..     None
.. }
> let nums = [1, 3, 5, 7, 9, 11, 13];
> binary_search(nums, 7)
Some(3)
```

That last line — a bare expression with no `let` — is shown as a value. The REPL prints `Some(3)` because that's what the expression evaluated to. Compare this to the file version, which had to wrap the result in `match` and `println` to see it. Showing a value never consumes it: a bare `nums` prints the vector and leaves `nums` where it was.

### Cells remember each other

The `fn binary_search` declaration is a *pure-items cell*: it adds a function to the session. Later cells can call it without redefining it. Same for `let nums = …` — that binding stays in scope for every cell that follows.

```text
> binary_search(nums, 100)
None
> binary_search(nums, 5)
Some(2)
```

`nums` is still here. So is `binary_search`. The REPL holds onto your work the same way a file's top-to-bottom order does, just one cell at a time.

### Re-declaring is allowed

You don't have to invent new names for retries:

```text
> let nums = [10, 20, 30, 40, 50];
> binary_search(nums, 30)
Some(2)
```

The second `let nums` *shadows* the first — same name, fresh binding. As in a file, shadowing ends nothing: the old vector stays alive, out of reach, until the session ends. This is what you'd want: experimenting in the REPL shouldn't pile up `nums1`, `nums2`, `nums_v3` in your head.

### Ownership crosses cells, honestly

This is the part most REPLs cheat on. They evaluate each cell in isolation and pretend ownership doesn't exist. Kāra doesn't pretend.

```text
> let owned = [1, 2, 3];
> let sum: i32 = owned.iter().sum();
> println(f"sum={sum}, owned still here: {owned.len()}");
sum=6, owned still here: 3
```

`owned.iter().sum()` borrows; the original is still yours. But:

```text
> let s: String = "hello".to_string();
> let taken = s;
> println(s);
error[E0500]: cell 13:1:9: value 's' moved here, used again here (moved at cell 12:1:13)
   |
 1 | println(s);
   |         ^
  help: clone 's' at the move site (`s.clone()`), drop `own` from the callee parameter if it only reads, or restructure to avoid reuse
```

The diagnostic doesn't just say *moved* — it tells you *which cell* the move happened in (cell 12, column 13: the `s` in `let taken = s;`) and suggests a fix. A refused cell changes nothing, so the session carries on as if you had never typed it. This is the [`UseAfterMove` notebook-aware tail](./ch12-ownership.md) at work; ownership in the REPL behaves exactly like ownership in a file, but the diagnostics know about your cell history.

Teaching ownership honestly from day one matters: when you graduate from REPL doodles to compiled `.kara` files, nothing has to be un-learned.

## Meta-commands

The REPL ships with a handful of `:command` helpers. Two are worth knowing right away.

### `:effects` — what does this session touch?

In a fresh `karac repl`:

```text
> fn read_config() -> String {
..     FileSystem.read_to_string("config.toml").unwrap()
.. }
> :effects
session effects: blocks, panics, reads(FileSystem)
  read_config: blocks, panics, reads(FileSystem)
```

The first line is the whole session's effects: its functions' and its cells'. Under it comes every function the session defined, with the effects it carries. This is the same effect analysis that the compiler runs on `.kara` files — you're just getting a live readout instead of waiting for a diagnostic to fire. We'll cover the effect system properly in [chapter 11](./ch11-effects.md); for now, treat `:effects` as a "what would I have to declare if this were a public API" lens.

### `:save` — graduate to a file

When the REPL session has earned its keep, hand it off to disk:

```text
> :save search.kara
session saved to search.kara
```

`:save` writes a real `.kara` file. The session's items become top-level definitions, the cells that ran become the body of `fn main()` (a refused cell is left out, and a bare expression is kept as `let _ = …;`, which runs it without printing), and any `:provide` scopes you opened are emitted as `with_provider[R](…, || { … })` blocks. The file `karac run`s without modification.

This is the natural lifecycle: prototype in the REPL, `:save` when the shape feels right, edit the file from there. The same compiler runs both ends; nothing gets translated.

## Which surface, when?

Both surfaces are first-class. As a rough rule:

- **Files** for anything with a real `main`, anything you'll come back to next week, anything you'd put under version control. They're cheap to start — `fn main() { … }` is the whole ceremony.
- **REPL** for learning the language, exploring a new crate, sanity-checking a one-liner, or shaping a function whose signature you're not sure about yet. `:effects` and the cell-history-aware diagnostics make it a teaching surface, not just a calculator.

Reach for whichever feels right. The compiler doesn't care which surface called it — your code's behavior is the same either way.

## What's next

You've seen Kāra running. The next few chapters introduce the building blocks — variables and types, functions, control flow — using whichever surface fits each example. We'll mostly show file form, because it's easier to read on a page, but every example also runs in the REPL if you'd rather experiment.
