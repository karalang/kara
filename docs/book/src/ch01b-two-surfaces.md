# Getting Started, Part 2: Two Surfaces

Kāra is one language with two everyday surfaces. You can save your code in a `.kara` file and run it with `karac run`, or you can paste it line by line into `karac repl` and watch each piece take effect immediately. Both surfaces run the same compiler, see the same diagnostics, and apply the same ownership rules. The difference is the rhythm: a file is a finished thought, the REPL is a thought in progress.

This chapter walks one example — a binary search over a sorted vector — through both surfaces side by side, so you can feel where each one shines.

> **Try without installing.** A browser playground at <https://play.kara-lang.org> runs the same compiler in your browser. If you'd rather read along than install, paste any example from this chapter there and the diagnostics will match what you'd see locally.

## The same program, on disk

Save this to `search.kara`:

```kara
fn binary_search(haystack: ref Vec[i32], needle: i32) -> Option[usize] {
    let mut lo: usize = 0;
    let mut hi: usize = haystack.len();
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
    let nums = vec![1, 3, 5, 7, 9, 11, 13];
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

- **`ref Vec[i32]`** says "I want to read this vector, not take ownership of it." The caller keeps `nums` and can use it again afterward.
- **`Option[usize]`** is the standard "maybe an index" return. Pattern-match on it; the compiler will warn you if you forget a case.
- **No allocator imports, no module declarations.** A `.kara` file with `fn main()` is a complete program.

## The same program, in the REPL

Now start the REPL:

```text
$ karac repl
Kāra REPL — :help for commands, :quit to exit.
karac> 
```

We'll build the same example cell by cell. Each line you submit is a *cell* — its own unit of evaluation, kept around so later cells can see it.

```text
karac> fn binary_search(haystack: ref Vec[i32], needle: i32) -> Option[usize] {
    ...     let mut lo: usize = 0;
    ...     let mut hi: usize = haystack.len();
    ...     while lo < hi {
    ...         let mid = (lo + hi) / 2;
    ...         let value = haystack[mid];
    ...         if value == needle { return Some(mid); }
    ...         else if value < needle { lo = mid + 1; }
    ...         else { hi = mid; }
    ...     }
    ...     None
    ... }
karac> let nums = vec![1, 3, 5, 7, 9, 11, 13];
karac> binary_search(nums, 7)
Some(3)
```

That last line — a bare expression with no `let` — is shown as a value. The REPL prints `Some(3)` because that's what the expression evaluated to. Compare this to the file version, which had to wrap the result in `match` and `println` to see it.

### Cells remember each other

The `fn binary_search` declaration is a *pure-items cell*: it adds a function to the session. Later cells can call it without redefining it. Same for `let nums = …` — that binding stays in scope for every cell that follows.

```text
karac> binary_search(nums, 100)
None
karac> binary_search(nums, 5)
Some(2)
```

`nums` is still here. So is `binary_search`. The REPL holds onto your work the same way a file's top-to-bottom order does, just one cell at a time.

### Re-declaring is allowed

You don't have to invent new names for retries:

```text
karac> let nums = vec![10, 20, 30, 40, 50];
karac> binary_search(nums, 30)
Some(2)
```

The second `let nums` *shadows* the first — same name, fresh binding. The old vector is dropped at the moment you re-declare. This is what you'd want: experimenting in the REPL shouldn't pile up `nums1`, `nums2`, `nums_v3` in your head.

### Ownership crosses cells, honestly

This is the part most REPLs cheat on. They evaluate each cell in isolation and pretend ownership doesn't exist. Kāra doesn't pretend.

```text
karac> let owned = vec![1, 2, 3];
karac> let sum: i32 = owned.iter().sum();
karac> println(f"sum={sum}, owned still here: {owned.len()}");
sum=6, owned still here: 3
```

`owned.iter().sum()` borrows; the original is still yours. But:

```text
karac> let s: String = "hello".to_string();
karac> let taken = s;
karac> println(s);
error: use of moved value `s`
  --> cell 3:1
   |
 1 | println(s);
   |         ^ value moved into `taken` in cell 2
   = the move happened in a previous cell; this cell sees the post-move state.
   = consider `let taken = s.clone();` if you need both bindings.
```

The diagnostic doesn't just say *moved* — it tells you *which cell* the move happened in and suggests a fix. This is the [`UseAfterMove` notebook-aware tail](./ch12-ownership.md) at work; ownership in the REPL behaves exactly like ownership in a file, but the diagnostics know about your cell history.

Teaching ownership honestly from day one matters: when you graduate from REPL doodles to compiled `.kara` files, nothing has to be un-learned.

## Meta-commands

The REPL ships with a handful of `:command` helpers. Two are worth knowing right away.

### `:effects` — what does this session touch?

```text
karac> fn read_config() -> String {
    ...     std::fs::read_to_string("config.toml").unwrap()
    ... }
karac> :effects
session effects: reads(Files), panics
  read_config: reads(Files), panics
```

Every function the session knows about, every effect it carries. This is the same effect analysis that the compiler runs on `.kara` files — you're just getting a live readout instead of waiting for a diagnostic to fire. We'll cover the effect system properly in [chapter 11](./ch11-effects.md); for now, treat `:effects` as a "what would I have to declare if this were a public API" lens.

### `:save` — graduate to a file

When the REPL session has earned its keep, hand it off to disk:

```text
karac> :save search.kara
wrote search.kara (4 cells, 1 fn, 1 let)
```

`:save` writes a real `.kara` file. The session's items become top-level definitions, the cell history becomes the body of `fn main()`, and any `:provide` scopes you opened are emitted as `with_provider[R](…) { … }` blocks. The file `karac run`s without modification.

This is the natural lifecycle: prototype in the REPL, `:save` when the shape feels right, edit the file from there. The same compiler runs both ends; nothing gets translated.

## Which surface, when?

Both surfaces are first-class. As a rough rule:

- **Files** for anything with a real `main`, anything you'll come back to next week, anything you'd put under version control. They're cheap to start — `fn main() { … }` is the whole ceremony.
- **REPL** for learning the language, exploring a new crate, sanity-checking a one-liner, or shaping a function whose signature you're not sure about yet. `:effects` and the cell-history-aware diagnostics make it a teaching surface, not just a calculator.

Reach for whichever feels right. The compiler doesn't care which surface called it — your code's behavior is the same either way.

## What's next

You've seen Kāra running. The next few chapters introduce the building blocks — variables and types, functions, control flow — using whichever surface fits each example. We'll mostly show file form, because it's easier to read on a page, but every example also runs in the REPL if you'd rather experiment.
