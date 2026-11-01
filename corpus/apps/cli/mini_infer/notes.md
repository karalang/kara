# mini_infer

A type checker for a subset of OCaml, the part of a compiler that an
editor's language server runs on every keystroke. It lexes and parses `.ml`
source, infers the type of every expression with Hindley-Milner inference,
reports the first type error in each top-level item in OCaml's words, warns
about matches that miss a case or have a case that can never run, and
answers "what is the type under the cursor".

```
$ snip let rec map f l = match l with [] -> [] | x :: xs -> f x :: map xs
snip.ml:1:65-1:67: error: this expression has type 'a list but an expression was expected of type 'a -> 'b
   1 | let rec map f l = match l with [] -> [] | x :: xs -> f x :: map xs
     |                                                                 ^^
```

## What it does

`mk.py` writes six programs to `data/`:

- `lists.ml`: a list library (map, folds, sort, zip, partition), each
  function polymorphic.
- `shapes.ml`: records, record update, variants with several arguments,
  and the three warnings an editor shows while you type.
- `json.ml`: a JSON value, its printer, and path queries.
- `interp.ml`: an interpreter and a checker for a small language, with
  mutual recursion, a two-parameter `result` type and a type abbreviation.
- `bank.ml`: a ledger with nine mistakes a reviewer would catch. Each item
  fails on its own and the items after it still check.
- `queue.ml`: a functional queue, a binary search tree used as a map, and a
  leftist heap.

`data/jobs.txt` holds 119 jobs. `mini_infer.kara` runs them one task per
job and prints them in job order.

- **`check`**: every top-level item's inferred type (`val map : ('a -> 'b)
  -> 'a list -> 'b list`), with each error and warning where it occurs,
  under a caret line like the one above.
- **`hover`**: the type of the innermost expression or pattern at a
  line and column, the way an editor's hover does.
- **`snip`** (107 jobs): one-line programs. They cover inference without
  errors, type clashes and the occurs check, unbound names with a
  did-you-mean, constructor and type arity, records, patterns that bind a
  name twice, warnings 8, 11 and 26, and errors from the lexer and parser.

```
$ check bank.ml
...
bank.ml:44:21-44:24: error: unbound value map (did you mean max?)
  44 | let summary accts = map (fun a -> a.owner) accts
     |                     ^^^
bank.ml:48:18-48:25: error: this function has type account -> int -> account. It is applied to too many arguments
...
```

## How the checker works

- **Inference is OCaml's.** Type variables are union-find nodes with a
  level. A `let` raises the level, and generalisation quantifies the
  variables still above the outer level, so no environment walk is
  needed. Every `let` is generalised, since there is no mutable state to
  need the value restriction.
- **Errors name the innermost clash.** A failed unification undoes the
  links it made, so the message prints the two types as they were, and a
  second line names the innermost pair that clashed
  (`type bool is not compatible with type int`) or the variable that
  occurs inside its own type.
- **The expected type reaches a `fun`'s parameters first**, as OCaml's
  `type_expect` does. That puts the error in the `map` example above on
  `xs`, not on the whole function.
- **The match warnings use Maranget's algorithm** ("Warnings for pattern
  matching", 2007). Exhaustiveness asks whether a row of wildcards is
  useful against the arms and builds the missing case as it goes, for
  example `_ :: _ :: _` or `{x = _; y = false}`. A case is unused when it
  is not useful against the arms above it. Guarded arms count for neither.
- **Types and patterns are arenas.** Types are indices into parallel
  vectors in both the port and the Python mirror, so the two allocate the
  same variables in the same order and print the same names.

The oracle is `oracle.py`, a Python mirror of the same checker that was
written first. No OCaml toolchain is installed here, so the messages follow
OCaml 5's wording rather than coming from `ocaml` itself.

## Compiler gaps

One row, filed in the kara ledger:

- B-2026-11-01-7: a call or operator reads a `Copy` field of a place after a
  later argument has moved or written that place. `wrap(body.span,
  vec![body])` is refused when run or built ("use of _3.0, which may have
  been moved"), and `pair(c.n, bump(mut c))` prints `11 11` instead of
  `1 11`, against core-semantics §8.1. `karac check` accepts both. The
  parser copies the span into a local first, with a comment naming the
  row.

The Mend loop otherwise: `karac fix` applied 19 changes on 17 lines. It
dropped a `*` on 11 lines where a loop bound an `i64` or a `bool` by copy,
and added `.clone()` on 6 where a borrowed `String` was moved. The hand
edits all followed a diagnostic:

- `NK` became `Nk` and `SP` became `SPat`, because type names must be
  PascalCase.
- `p.name.parse[i64]()` became `i64.parse(p.name)`, after the diagnostic
  said `i64` is a type, not a function.
- Four `Map` lookups bind `Some(i) => *i`, because `get` returns a
  reference and the checker would otherwise hold a borrow of `self`.
- Sixteen discarded unification results became `let _ = ...`.
- `Span` became `Loc`, because it shadowed a prelude type.

After that, the interpreter matched all 674 lines of the oracle on its
first run. The LLVM, ASan and native builds matched at 1 and 4 threads,
and the auto-par binary matched on five runs.

## Scope

- The language is OCaml without modules, exceptions, references, arrays,
  floats, chars, labels, or-patterns and `as`. A module path such as
  `List.length` parses and reports `unbound module List`.
- A sequence `e1; e2` requires `e1 : unit`, as `-strict-sequence` does.
- A type abbreviation is expanded where it is used, so messages print
  `(string * value) list` where OCaml would print `env`.
- Each item reports only its first error, and its names then get the type
  `'a` so the items after it do not report the same mistake again.
- Nothing here needs the data library (docs/library/data.md).

## Running

```bash
karac run mini_infer.kara                         # interpret
karac build mini_infer.kara                       # native binary
python3 -s -P -B mk.py                            # regenerate data/
python3 -I -B oracle.py > expected.out            # regenerate expected.out
python3 -I -B gen_kara.py                         # regenerate the embedded data
```
