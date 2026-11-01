# mini_regalloc

The back end of a compiler, for [mini_ssa](../mini_ssa)'s three-address
IR. It selects instructions for a small load/store machine, computes
liveness, live intervals and the interference graph, and allocates K
registers two ways: Poletto and Sarkar's linear scan, the allocator JITs
use, and Chaitin's graph colouring with Briggs's optimistic colouring, the
classic ahead-of-time one. A simulator runs the result, so every `run` job
checks both allocations against an interpreter of the IR.

```
$ colour abi.ir chain 3
; %c.0 r1, %k r2, %m r1, %n r2, %r r1, %s r1, %z r1
chain:  # frame 3
.entry:
  mv r2, a0
  li r1, 0
  seq r1, r2, r1
  beqz r1, rec
.base:
  li a0, 0
  ret
.rec:
  subi r1, r2, 1
  mul r2, r2, r2
  mv a0, r1
  st r2, [sp+1]
  call chain
  ld r2, [sp+1]
  mv r1, a0
  add r1, r1, r2
  mv a0, r1
  ret
; spilled 0, loads 0, stores 0, saves 1, moves removed 0, size 16
```

`k = n * n` is live across the recursive call, and every register is
caller-saved, so it is stored before the call and reloaded after.

## The machine

- `r1` .. `rK` are the registers the allocators hand out. `t0` and `t1`
  are scratch registers for spilled values, and `a0` .. `a3` carry the
  arguments and the result. A call may change every register, as on a
  machine where all registers are caller-saved.
- The instructions are `li`, `mv`, the three-register arithmetic, bitwise,
  shift and comparison forms (`add`, `seq`, `slt`, ...), the immediate
  forms of `+ - & | ^ << >>` (`addi`, `slli`, ...), `bnez`, `beqz`, `j`,
  `call`, `ret`, `print`, and `ld` / `st` on stack slots (`[sp+3]`).
- Integers are 32-bit and wrap, and dividing by zero traps, as in the IR.

## What it does

`mk.py` writes six programs to `data/`. Four come from mini_ssa
(`arith.ir`, `loops.ir`, `swap.ir`, `calls.ir`), and two are new:

- `pressure.ir`: more live values than registers: eight products summed
  and crossed, six accumulators in a loop, Fibonacci by 2x2 matrix
  powers, four values live across four calls, and a hash loop.
- `abi.ir`: the calling convention: four-argument calls, arguments in a
  different order from the parameters, recursion with a value live across
  the call, and output inside a loop of calls.

`data/jobs.txt` holds 200 jobs. `mini_regalloc.kara` runs them one task
per job and prints them in job order.

- **`sel`**: the machine code over virtual registers (`%x`).
- **`live`**: each virtual register's live interval, and which values are
  live across each call.
- **`graph`**: the interference graph, with each node's degree and spill
  cost.
- **`linear`** and **`colour`**: where each value lives, the final code,
  and what the allocator cost: values spilled, loads, stores, saves around
  calls, moves removed and code size.
- **`compare`**: both allocators at K = 2, 3, 4, 6 and 8.
- **`run`** (89 jobs): the IR interpreted, then the program allocated both
  ways and simulated, with the result, the output, the instruction count
  and the memory traffic. An allocation that disagrees with the IR prints
  `MISMATCH`; none does.
- **`snip`** (29 jobs): one-function programs, for instruction selection's
  corner cases, the calling convention's limit, K = 1, traps and parse
  errors.

```
$ compare pressure.ir spread
K=2 linear: spilled 10, loads 23, stores 16, saves 0, size 73
K=2 colour: spilled 10, loads 17, stores 10, saves 0, size 61
...
K=6 linear: spilled 3, loads 6, stores 3, saves 0, size 43
K=6 colour: spilled 3, loads 6, stores 3, saves 0, size 43
```

The two spill the same number of values here, but not the same values:
linear scan spills whichever interval ends last, however often it is
used, while colouring weighs each value's uses (10 per loop level)
against its neighbours, so its spills cost fewer loads and stores.

## How it works

- **Instruction selection** is one pass per IR instruction. A constant
  right operand of `+ - & | ^ << >>` becomes an immediate, a constant
  left operand of a commutative operator is swapped to the right, and any
  other constant is loaded with `li` into a fresh `%c.N`. A branch to the
  next block in the layout falls through. Unreachable blocks are dropped.
  A name read before any assignment reaches it is set to 0 at entry, which
  is what the IR says it reads.
- **Liveness** is the usual backward data-flow over the machine code's
  virtual registers. Each instruction reads at an even position and
  writes at the odd one after, so a value whose last use is an
  instruction's operand can share a register with that instruction's
  result.
- **Linear scan** (Poletto and Sarkar, "Linear Scan Register Allocation",
  1999) sorts the intervals by start, expires the active ones that ended,
  and when all K registers are taken spills whichever of the new interval
  and the active ones ends last.
- **Graph colouring** removes a node of degree below K while there is one
  (Chaitin, 1982), else the node with the least spill cost per neighbour.
  It colours in reverse order and spills a node only when its neighbours
  have used every colour (Briggs, Cooper and Torczon, 1994).
- **Rewriting** loads a spilled value into `t0` or `t1` before each use
  and stores it from `t0` after each definition, so allocation never has
  to run again, as on early RISC compilers that kept scratch registers
  back. Each register holding a value live across a call is saved to its
  own slot before the call and reloaded after. A move from a register to
  itself is removed.

The oracle is `oracle.py`, a Python mirror of the same back end that was
written first, and the simulator's agreement with the IR interpreter on
every `run` job is a second check on both.

## Compiler gaps

One row, filed in the kara ledger:

- B-2026-11-01-11: swapping two operands, the first way I wrote it was
  `(x, y) = (y, x)`. On two `String` bindings, `karac check` gives the
  right fix (drop the parentheses), but also four move errors that only
  exist because the statement was not read as a swap, each with a
  `.clone()` fix. `karac fix` applies the clones instead of the real fix,
  which leaves `(x.clone(), y.clone()) = (y.clone(), x.clone())`, and a
  second run leaves `x.clone(), y.clone() = ...`, which no fix repairs.
  With `i64` bindings the real fix applies alone. The port uses the form
  the diagnostic names, `x, y = y, x`, which works.

The Mend loop otherwise: `karac check` asked for two renames (`RK` and
`MI` are not PascalCase), and `karac fix` then had nothing to change. The
interpreter matched all 3,291 lines of the oracle on its first run, and
the LLVM, ASan and native builds matched at 1 and 4 threads.

## Scope

- No coalescing beyond removing a move whose ends got the same register,
  no live-range splitting, and no callee-saved registers.
- At most four arguments, since there are four argument registers; a
  function or call with more is an error, as a calling convention without
  stack arguments would make it.
- Nothing here needs the data library (docs/library/data.md).

## Running

```bash
karac run mini_regalloc.kara                      # interpret
karac build mini_regalloc.kara                    # native binary
python3 -s -P -B mk.py                            # regenerate data/
python3 -I -B oracle.py > expected.out            # regenerate expected.out
python3 -I -B gen_kara.py                         # regenerate the embedded data
```
