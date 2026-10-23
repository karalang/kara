# mini_constraint

A port of [python-constraint](https://pypi.org/project/python-constraint/)
1.4.0 (2-clause BSD, Gustavo Niemeyer): a **constraint-satisfaction problem
(CSP) solver**. A CSP is a set of variables, each with a finite domain of
candidate values, plus constraints the values must jointly satisfy; the solver
enumerates every assignment that satisfies all of them. CSP solvers are the
engine behind puzzle solvers (Sudoku, N-queens), timetabling and scheduling,
resource allocation, and configuration validation.

## What it does

`mk.py` writes `data/jobs.tsv`: one CSP per line, as a space-separated spec.

```
queens N              the N-queens problem on an N×N board
rooks N               N non-attacking rooks on an N×N board
color V K e e e ...   K-colour a graph of V nodes; each e is an edge "a-b"
alldiff V d0 d1 ...   V variables, all different; each dI a comma-separated domain
```

`oracle.py` builds each problem with the library's own `Problem` /
`BacktrackingSolver`, `FunctionConstraint` and `AllDifferentConstraint`,
enumerates every solution with `getSolutions()`, and prints:

```
<job line> => <count>
<v0 v1 ... v_{V-1}>        one line per solution, values in variable order,
...                        in the solver's own enumeration order
```

`mini_constraint.kara` reproduces it byte-for-byte on every backend.

## How the port works

The port reproduces the library's `BacktrackingSolver` exactly — not just the
set of solutions, but the **sequence** in which they are emitted. That sequence
is a fingerprint of the whole search, so matching it byte-for-byte means
reproducing every mechanism the solver uses:

- **Variable ordering** — a combined *minimum-remaining-values* and *degree*
  heuristic: at each step the unassigned variable chosen is the one with the
  most constraints, then the smallest remaining domain, then the lowest id
  (the library builds `(-degree, domain_size, variable)` tuples and sorts; the
  port selects the minimum of the same tuple, which is equivalent).
- **Value ordering** — the library pops candidate values off the *end* of the
  domain list, so values are tried in reverse.
- **Forward checking** — on each assignment, values that can no longer satisfy
  a constraint are pruned from the domains of the still-unassigned variables
  (the generic single-unassigned check for the binary constraints, and
  `AllDifferentConstraint`'s "hide every already-used value" pass).
- **The domain state stack** — pruned values are *hidden*, not deleted, and are
  restored when a choice is undone; crucially they are appended back at the
  **end** of the domain, which reorders it, and that reordering changes which
  value is tried next. The port mirrors the library's `pushState` / `popState`
  / `hideValue` list operations exactly, so the reordering is identical.

Constraints are represented as concrete tagged kinds rather than arbitrary
callables: a binary "not equal" (graph colouring, rooks), a binary queens
constraint (`|row_i − row_j| ≠ |col_i − col_j|` and `row_i ≠ row_j`), and
all-different. These are exactly the `FunctionConstraint` lambdas and the
`AllDifferentConstraint` the examples use, so the library-side oracle builds the
identical problems. The whole solver is integer- and list-based, so the four
backends agree by construction; there is no floating point.

## Scope

- The three constraint kinds here (not-equal, the queens relation,
  all-different) cover the N-queens, rooks, graph-colouring and Latin-square /
  all-different families. The library's other solvers (`RecursiveBacktracking`,
  `MinConflicts` — the last is randomised) and its other prepackaged
  constraints (`ExactSum`, `MaxSum`, `InSet`, …) are not exercised; the
  `BacktrackingSolver` is the default and the one these problems use.
- Variables are integers `0..V−1` and values are integers, which is what every
  problem here needs; the library accepts arbitrary hashable variables and
  values, but the heuristic's `variable` tie-break then depends on their
  ordering, so the integer case is the well-defined one to reproduce.

The port was checked against the library on every backend over the job list,
and separately during development over 16 problems — N-queens 4–8, rooks, 2- and
3-colourings of a 5-cycle, K4 and the Petersen graph (satisfiable and not), and
all-different over full and restricted domains — with the **full solution
enumeration** (order included) compared each time.

The vendored library package imports only the standard library; `vendor/` is
otherwise left untouched.

## Running

```bash
karac run mini_constraint.kara             # interpret
karac build --mir mini_constraint.kara     # native binary
python3 -s -P -B mk.py                       # regenerate data/jobs.tsv
python3 -I oracle.py                          # regenerate expected.out
python3 -s -P -B gen_kara.py                  # regenerate the embedded jobs
```
