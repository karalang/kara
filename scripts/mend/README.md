# Rust-ism audit

`rustism_audit.py` measures how `karac check` and `karac fix` answer the
mistakes a writer with Rust habits makes in Kāra. It takes corpus programs
that check clean and print their expected output, makes ONE mistake in each,
and asks:

1. **One new diagnostic**: does `karac check` report exactly one diagnostic
   the original did not (an error, or a warning for a mistake that is legal
   but redundant)?
2. **Fixed, same output**: does `karac fix`, run until the text stops
   changing (at most three passes), leave a program that checks with no new
   diagnostic and prints what the original printed on `karac __mir-run`?

A mistake that is valid Kāra with Rust's meaning (`#[derive(Debug)]`,
`Some(ref x)`) passes when nothing new is reported, or when what is reported
is right and its fix restores the program. `while let Some(x) = v.pop()` is
valid Kāra with Rust's meaning and has no injection.

**This is not a Mend rate.** The mistakes are made by a script that knows the
language, not by a blind model, so the table measures the fix surface for
known mistakes. The machine-fix rate of blind authorship is
`examples/mend/harness/mend_batch.py`'s, and only that one may be quoted as
one.

## Running it

```bash
cargo build --release --bin karac
python3 scripts/mend/rustism_audit.py --json out.json --markdown table.md
python3 scripts/mend/rustism_audit.py --only tuple_struct   # one mistake
```

The programs are the 10 apps and 10 katas with the most places for a
mistake, then more from `corpus/{apps,katas,examples,design,core}` for any
mistake those have fewer than 5 places for (`--programs`,
`--min-per-mistake`). Each program runs in a scratch directory, so one that
writes files writes them there. About 25 minutes.

## Results

At the commit that added this file, over 39 programs:

| Mistake | Programs | One new diagnostic | Fixed, same output | Pass |
|---|---|---|---|---|
| `Enum::Variant` path | 19 | 19/19 | 19/19 | 19/19 |
| `String::new()` / `Vec::new()` | 20 | 20/20 | 20/20 | 20/20 |
| `&x` argument | 20 | 20/20 | 20/20 | 20/20 |
| `&mut x` argument | 9 | 9/9 | 9/9 | 9/9 |
| `!cond` | 11 | 11/11 | 11/11 | 11/11 |
| `vec![..]` | 5 | 5/5 | 5/5 | 5/5 |
| `impl<T>` | 5 | 5/5 | 5/5 | 5/5 |
| `fn f<T>` | 5 | 5/5 | 5/5 | 5/5 |
| `.collect::<Vec<T>>()` | 16 | 16/16 | 16/16 | 16/16 |
| `.collect::<Vec<_>>()` | 16 | 16/16 | 10/16 | 10/16 |
| `let mut` never reassigned | 20 | 0/20 | 20/20 | 0/20 |
| `Some(ref x)` | 17 | 3/17 | 17/17 | 17/17 |
| `x as T` where `x` is a `T` | 20 | 20/20 | 20/20 | 20/20 |
| `println!("{}", x)` | 20 | 20/20 | 20/20 | 20/20 |
| `#[derive(Debug)]` | 20 | 0/20 | 20/20 | 20/20 |
| `pub(crate) fn` | 20 | 20/20 | 20/20 | 20/20 |
| `_ =>` on an exhaustive enum match | 12 | 12/12 | 12/12 | 12/12 |
| `?` in a `main` returning `()` | 5 | 5/5 | 5/5 | 5/5 |
| tuple struct `S(T)` and `.0` | 8 | 8/8 | 8/8 | 8/8 |

`Some(ref x)` and `#[derive(Debug)]` are valid Kāra: the three `Some(ref x)`
diagnostics are a correct ownership error (moving out through the `ref`),
which `karac fix` repairs.

The two short of a full pass are rows in `docs/bugs-v2.jsonl` (tagged
`probe:rustism-audit`):

- **`let mut` never reassigned** draws nothing: Kāra has no lint for an
  unwritten `let mut`, and adding one is a spec change.
- **`.collect::<Vec<_>>()`**: the fix removes the turbofish and annotates the
  binding when nothing else decides the target. In 6 programs a later use
  decides it (the `Vec` is passed to a `Vec[T]` parameter), which the type
  checker accepts and MIR does not lower yet.

Before the fixes in that commit, the same audit found no machine-applicable
fix for `pub(crate)` (0/20), an unreachable `_ =>` arm (0/11), `?` in a unit
`main` over an error that is no `Error` (0/5), tuple structs (0/9),
`.collect::<Vec<_>>()` (0/17), and `impl<T: Add<Output = T>>` (the `=` inside
stopped the bracket fix). Its `?` injection also found eleven library error
types that did not implement `Error`.
