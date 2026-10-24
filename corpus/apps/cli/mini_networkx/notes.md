# mini_networkx

[networkx](https://networkx.org) 3.4.2 (BSD-3-Clause), Python's graph
library, ported to Kāra: its `Graph` and `DiGraph` classes and the thirty
algorithms that a road map, a package dependency graph, a site's link
graph, a pipe network and a grid of streets call on. The port runs 366
queries over 15 graphs and prints what networkx returns for each.

```
dijkstra roads ashford fenford: dist 186: ashford > elmham > barham > torham > ulley > calford > kelley > ulmouth > morton > fenford
cuts roads: 6 cut points: fenton torham ashford elmham penton ulley
longest deps: length 145: libcore > libutil > liblog > libnet > libyaml > libcrypt > libtmpl > libadmin > libchat > appshop
reduce deps: 74 kept, 19 removed: libcore>liblog libcore>libhttp libcore>libdb libcore>libdiff libutil>libhttp libutil>libcli ...
cycle loop: cycle of 9: libutil > liblog > libnet > libyaml > libcrypt > libtmpl > libadmin > libchat > appshop > libutil
scc web: 16 components: 1 privacy | 1 terms | 1 press | 1 post12 | 1 post02 | 33 about ...; fnv 686ee446
maxflow pipes src sink: value 36; sink side 20; cut src>j0a src>j0b
euler ring: circuit of 60 edges, 61 nodes: r0c0 r4c0 r4c5 r4c4 r4c3 r4c2 r4c1 r4c0 r3c0 r3c5 ...; fnv c3459d1a
```

## What it does

`mk.py` writes `data/jobs.txt`: the graphs as `graph`, `n` and `e` lines,
then the queries. The graphs are

- `roads`, 42 towns joined to their nearest neighbours by weighted roads,
  with four spur towns that hang off the network;
- `deps`, 46 packages as a DAG, an edge from a dependency to each
  package that needs it, weighted by build minutes;
- `web`, 48 pages linking to each other, three of them with no links out;
- `pipes`, a source, five layers of junctions and a sink, with
  capacities, and a pair of pipes both ways in some layers;
- `ring`, a 5 by 6 torus of streets, where every corner has degree 4;
- `g0` to `g9`, random graphs of 14 to 27 nodes: trees, bipartite graphs
  and sparse to dense ones.

The queries are shortest paths by hops and by weight, the distances in
the order Dijkstra settles them, BFS and DFS orders, components and
strongly connected components, the condensation, the three topological
orders, the longest path, transitive reduction, ancestors and
descendants, `find_cycle`, minimum and maximum spanning trees, maximum
flow with the minimum cut, articulation points, bridges, core numbers,
two greedy colourings, triangles, bipartite sides, eccentricity by hops
and by weight, Euler circuits and all simple paths under a cutoff. Some
queries change a graph (`copy`, a closed road, a removed town, a new
edge, an edge that makes a cycle) and the queries after them see the
change; some fail, and print networkx's message (an unknown town, no
path, a cycle, a graph that is not connected).

`oracle.py` runs the same jobs through networkx itself and wrote
`expected.out`. Long lists print their first entries and an FNV-1a hash
of the whole list, so every node of an order is checked.

## How the port works

A graph keeps its nodes in slots, in insertion order, with a map from
name to slot; each slot has its neighbours (successors and predecessors
for a `DiGraph`) as `(node, weight)` lists in insertion order, as
networkx's nested dicts are. Removing a node kills its slot, and adding
the name again makes a new slot at the end, as deleting and re-inserting
a dict key does. `copy()` adds the nodes and then calls `add_edge` for
every adjacency entry in order, like networkx's, so a copy of an
undirected graph can list a node's neighbours in a different order from
the original. `Graph.edges()` yields an edge from the earlier of its two
nodes.

Every algorithm follows networkx's code, since its outputs depend on the
order it visits things: the bidirectional BFS expands the smaller
fringe; Dijkstra's heap holds `(distance, count, node)`; Tarjan is the
library's non-recursive version; Kahn's algorithm runs a generation at a
time; the lexicographic sort keeps its `(key, id, node)` heap; the
longest path takes the first best predecessor; transitive reduction
memoises and frees each neighbour's descendants; `find_cycle` walks
`edge_dfs` and unwinds its active path as networkx does; Kruskal sorts
stably (the reverse sort for a maximum tree is stable too); Edmonds-Karp
builds networkx's residual network and finds augmenting paths with its
bidirectional BFS; core numbers use the bin sort that swaps nodes in the
list it walks; DSATUR recounts saturation each step and breaks ties by
degree and then node order; and the Euler circuit walks the first
remaining edge out of the top of its stack.

Bridges are the one place the port takes another route: networkx finds
them as the edges in no chain of a chain decomposition, and the port
takes the biconnected components of a single edge, which are the same
set, then lists them in `G.edges()` order as networkx does.

## Compiler gaps

None. `karac check` asked for 13 `mut` markers at call sites that pass a
local to a `mut ref` parameter (`heap_push(mut fringe, ...)`,
`sort_names(mut names)`), and `karac fix` applied all 13. After that the
MIR interpreter matched the oracle on its first run, and so did the LLVM
backend, the ASan build and the native binary, byte for byte with
nothing on stderr.

## Scope

The port covers the classes and algorithms above for simple graphs with
an integer `weight` on every edge (also the capacity for flows).
Multigraphs, node and graph attributes, float weights, PageRank and the
other centralities, matchings, isomorphism, generators, drawing and I/O
are left out. One networkx quirk is not ported: `all_simple_paths` with a
target that is not a node treats the target as an iterable of nodes, so
a string target searches for its characters; the port reports that the
target is not in the graph, and no job asks for it.

## Running

```bash
python3 -s -P -B mk.py                 # writes data/jobs.txt
NETWORKX_SRC=<unpacked networkx-3.4.2> python3 -I -B oracle.py > expected.out
python3 -I -B gen_kara.py              # the jobs_text() function at the end of the .kara
karac run mini_networkx.kara
karac build --mir mini_networkx.kara && ./mini_networkx
```

`oracle.py` imports an installed networkx 3.4.2, or the one under
`NETWORKX_SRC` (`pip download --no-deps --no-binary :all:
networkx==3.4.2` and unpack it). About 5.5 s on the MIR interpreter (a
debug build of `karac`) and 0.01 s built. The program writes
`data/jobs.txt`, reads it back and removes it.
