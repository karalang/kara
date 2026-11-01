# mini_ssa

The middle end of an optimizing compiler, over a small three-address IR.
It builds each function's control-flow graph, computes dominators,
dominance frontiers, natural loops and liveness, puts the function in SSA
form, optimizes it with sparse conditional constant propagation, copy
propagation, dead code elimination and CFG cleanup, and takes it back out
of SSA. An interpreter runs every stage, so each `run` job shows that the
optimized program computes what the source did.

```
$ out swap.ir lost
func lost(n.0) {
entry:
  x.1 = 0
  jmp loop
loop:
  x.2 = x.1 + 1
  c.0 = x.2 < n.0
  br c.0, loop_loop, done
loop_loop:
  x.1 = x.2
  jmp loop
done:
  ret x.1
}
```

That is the lost-copy problem: the copy for the loop's phi has to go on
the back edge, not at the end of `loop`, or `done` would return the new
value instead of the old one. The back edge is critical (its source has two
successors and its target two predecessors), so it is split into
`loop_loop` first.

## The IR

```
func gcd(a, b) {
entry:
  jmp test
test:
  z = b == 0
  br z, done, step
step:
  t = a % b
  a = b
  b = t
  jmp test
done:
  ret a
}
```

Integers are 32-bit and wrap. `/` and `%` truncate toward zero, and
dividing by zero traps. The instructions are `x = a op b` (arithmetic,
bitwise, shifts and comparisons), `x = a`, `x = call f(args)`,
`call f(args)`, `print a`, `br c, L1, L2`, `jmp L` and `ret [a]`. A name
read before any assignment reaches it reads 0. `#` starts a comment. The
entry block cannot be a jump target, so its first instruction runs once.

## What it does

`mk.py` writes six programs to `data/`:

- `arith.ir`: gcd, fast exponentiation, Collatz steps, a classifier with
  a join of four paths, binary-search square root, and a function that
  shows the wrapping.
- `consts.ir`: what SCCP finds and a plain constant folder does not: a
  branch on a debug flag, a loop variable that never changes because the
  code that would change it is unreachable, a division by zero that must
  stay, and a block nothing jumps to.
- `loops.ir`: nested loops, trial division with an early exit, Fibonacci,
  a search that leaves its loop from the middle, and two loops in a row.
- `swap.ir`: the swap and lost-copy problems, a three-way rotation, and
  a loop whose exit is inside it.
- `calls.ir`: recursion, mutual recursion, Ackermann, output, a loop
  that never ends and a recursion that never ends.
- `warn.ir`: a name that may be read unset, a typo, and dead stores.

`data/jobs.txt` holds 228 jobs. `mini_ssa.kara` runs them one task per
job and prints them in job order.

- **`cfg`**: the reverse post-order and each block's predecessors and
  successors, with a note for each unreachable block.
- **`dom`**: each block's immediate dominator and dominance frontier, the
  dominator tree, and each natural loop's back edges and body.
- **`live`**: live-in and live-out sets, with a warning for a name live on
  entry that is not a parameter.
- **`ssa`**, **`opt`**, **`out`**: the function in SSA form, after the
  optimizer (with what it folded and removed), and after leaving SSA.
- **`run`** (59 jobs): the program run as written, in SSA form, optimized
  and out of SSA, with the result, the output and the step count of each.
  A stage that disagrees with the source prints `MISMATCH`; none does.
- **`snip`** (50 jobs): one-function programs, for the corner cases and
  every parse error.

```
$ run consts.ir steady 9
source: returned 10 after 61 steps
ssa: returned 10 after 61 steps
opt: returned 10 after 40 steps
out: returned 10 after 50 steps
```

A phi costs no step, because it is not an instruction a machine runs: the
copies that leave SSA are its real cost, which is why `out` takes more
steps than `opt`.

## How it works

- **Dominators** use Cooper, Harvey and Kennedy's iterative algorithm ("A
  Simple, Fast Dominance Algorithm", 2001) over the reverse post-order,
  and dominance frontiers use their walk up from each join's
  predecessors.
- **SSA construction** is Cytron et al.'s: phis go on the iterated
  dominance frontier of each name's definitions, and renaming walks the
  dominator tree with a stack of names per variable. It is semi-pruned
  (Briggs et al.): only a name some block reads before it writes it gets
  phis. SSA names are `x.0`, `x.1`, ...; the IR does not allow `.` in a
  name, so they cannot collide with one.
- **SCCP** is Wegman and Zadeck's: values are unknown, a constant, or
  varying, and only edges a constant branch can take make a block
  executable. It sweeps the executable blocks in reverse post-order until
  nothing changes, which reaches the same fixpoint as the work-list form.
  A division that would trap is varying, so it is never folded away.
- **Cleanup** propagates copies, removes instructions whose values nobody
  needs (a division by a variable is kept, since it may trap), turns a
  phi with one distinct argument into a copy, threads jumps through empty
  blocks and merges a block into a predecessor that has no other
  successor.
- **Leaving SSA** splits each critical edge into a phi, then turns each
  predecessor's phi arguments into a parallel copy and sequentializes it:
  a copy whose destination nothing else still reads goes first, and a
  cycle such as `a, b = b, a` goes through a temporary `$t0`.

The oracle is `oracle.py`, a Python mirror of the same passes that was
written first, and the four stages' agreement on every `run` job is a
second check on both.

## Compiler gaps

None found. The Mend loop: `karac fix` made three changes, dropping a
`ref` written at a call site and adding `.clone()` where a `for` loop
over a vector bound its elements by reference and the code moved a
field out. `karac check` then reported nothing, and the interpreter
matched the oracle on its first run.

Probed and working: a `Set` keyed by a pair of `String`s, `==` on
`Vec[String]`, an assignment whose right side reads the place it assigns
(`o.name = r.fresh(o.name)`, `x = sub[x.name.clone()].clone()`), a
`break` from inside a `match` arm, and a counter through `mut ref i64`.

## Scope

- No alias analysis, memory, or global value numbering: the IR has
  scalars only, and the optimizer does not simplify `x + 0`.
- SSA construction warns once per name when a read sees no assignment,
  or when a join reads a name that one of its incoming edges never set,
  as `maybe` and `typo` in `warn.ir` do. Like a C compiler's
  "may be used uninitialized", it does not prove the path can run.
- Nothing here needs the data library (docs/library/data.md).

## Running

```bash
karac run mini_ssa.kara                           # interpret
karac build mini_ssa.kara                         # native binary
python3 -s -P -B mk.py                            # regenerate data/
python3 -I -B oracle.py > expected.out            # regenerate expected.out
python3 -I -B gen_kara.py                         # regenerate the embedded data
```
