# mini_graphlib

Python 3.13's `graphlib.TopologicalSorter` as a generic Kāra type, ported
from `graphlib.py`, and two programs that use it: a CI runner that starts
a pipeline's jobs on N workers as their needs finish, and a package
installer that orders a package's dependencies.

The run writes a 13-job CI pipeline and a 16-package index into `data/`,
then runs `data/jobs.tsv`, 28 jobs with tab-separated fields. Each job
prints `== ` and its fields joined by ` | `, then its output.

The jobs cover:
- `order`: `static_order` on small graphs written `node:pred,pred;...`,
  including the empty graph, a node depending on itself, a predecessor
  given twice, two separate chains, and cycles, where the error names
  the cycle `_find_cycle` finds;
- `waves`: the groups `get_ready` hands out between `done` calls;
- `steps`: a script of `prepare`, `ready`, `done:`, `add:` and `active`
  calls, so each `ValueError` comes out (not prepared, prepared twice,
  adding after prepare, an unknown node, a node not passed out yet, a
  node done twice), and a graph with a cycle where `get_ready` still
  hands out the nodes outside it;
- `ci`: the pipeline on 1, 3 and 8 workers, printing each start and
  finish with its time and how busy the workers were;
- `install`: the install order for three packages, a dependency cycle
  (printed the way it would be installed), and an unknown package.

**The port** follows `graphlib.py`: nodes are kept in the order they are
first seen, `add` counts predecessors and records successors, `prepare`
sets the ready list before looking for a cycle, and `_find_cycle` is the
same depth-first walk with a stack of successor positions. Errors are a
`GraphError[N]` enum that carries the node, and the job runner turns
them into Python's messages.

Not supported: `TopologicalSorter`'s `__bool__`, and nodes of any type
other than the one a sorter is made for.

**Oracle.** `expected.out` is Python 3.13.16's output for the same jobs,
made by `oracle.py` with the real `graphlib`. Run `mk.py` first; it
writes `data/`. `gen_kara.py` prints the Kāra data section from the same
files.

## Edits from mini_graphlib.orig.kara to mini_graphlib.kara

Author errors:
- `for p in f[1].split(",")` and `for j in ts.get_ready()?` moved each
  borrowed element into a `Vec`; both now loop over `.into_iter()`.
- `let (f, q, _) = running[k]` moved the job name out of the `Vec`; the
  comparison now reads `running[k].0` and `running[k].1`.

Compiler gaps:
- B-2026-10-18-26: on the MIR surfaces, `r.map_err(graph_err)?` with a
  named function stops with "'?' cannot propagate error 'F' as
  'String'". The six calls now pass a closure,
  `.map_err(|e| error_text(e))`, and `graph_err` is gone.

After those edits the program matched expected.out on both MIR surfaces.

Written 2026-10-18 by the real-world apps thread; karac at 79a6c2eb9a.
