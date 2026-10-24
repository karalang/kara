# pipeline/route_plan

A delivery routing job, the shape of a dispatch service's nightly plan. It
builds the road networks of four city districts, writes each as an
edge-list file, reads the files back and parses them, computes the shortest
drive between every pair of stops with Dijkstra, plans a tour of each
district, runs five reports per district, and books the tours onto one
shared fleet of three trucks.

- A district is a 48x48 street grid. Each street is open with
  probability 93% and takes 5 to 24 minutes; about 6% of the blocks also
  get a diagonal arterial of 8 to 32 minutes. Everything comes from a
  32-bit integer hash of the district's seed and the position.
- The edge-list file is a header line (`city north 48x48 4310 edges`) and
  one `u v minutes` line per road. The parser checks the header, the
  district name, every field and the edge count. A copy with one field
  damaged, and the east file read as the west district, show its errors.
- Each district has a depot at its centre and fourteen orders of 1 to 6
  units. Dijkstra runs over a binary heap of `dist * 4096 + node` keys, so
  ties go to the lower node and the result does not depend on how a heap
  orders equal keys.
- A tour is nearest neighbour from the depot, then 2-opt with first
  improvement in scan order until a pass changes nothing.
- The reports: the network (streets, closures, arterials, the degree
  histogram), the reach from the depot in two-hour bands, the tour, the
  longest leg traced back through Dijkstra's predecessors, and the demand
  with a checksum of the distance matrix.
- The fleet: each tour is cut into trips of at most 10 units, each back to
  the depot with 4 minutes per drop, and each trip goes to the truck that
  is free first.

Everything is integer arithmetic, so the backends and the Python mirror
`route_plan.py` agree by construction; the mirror wrote `expected.out`.

stmt-par: eight groups of statements that do not depend on each other, in
`run()` (`route_plan.orig.kara` lines):
- L635-L638 four network builds, each from its own seed;
- L641-L644 four `fs.write` calls, each with the `encode_city` call that
  builds its text;
- L656-L659 four reads, each with `?`;
- L664-L667 four `parse_city` calls. Each can fail with `?`, so a parallel
  run has to report the first failure in source order;
- L680-L683 four `make_stops` calls;
- L686-L689 four distance matrices, fifteen Dijkstra runs each. This is
  where most of the time goes;
- L693-L696 four `plan_tour` calls;
- L699-L718 twenty reports, five per district, that only read what the
  earlier groups made. The reach and longest-leg reports run Dijkstra
  again, so this group is the second hot one.

The four `dispatch` calls (L726-L729) are the case that must stay in
order: each takes `mut ref fleet`, and every trip goes to the truck the
earlier trips left free first, so a district booked out of order changes
every later truck assignment.

## Edits from route_plan.orig.kara to route_plan.kara

Author error (the diagnostic was right): the fleet summary moved a truck
out of the vector with `let t = fleet.trucks[i];`. It is
`let t = ref fleet.trucks[i];` now, the borrow the error lists first.

Compiler gap: `karac fix`'s machine edit for that error is not the borrow.
It adds `#[derive(Clone)]` to `Truck`, and a second run makes the line
`fleet.trucks[i].clone()`, a deep copy of every truck's trip list; the
first message even says no `.clone()` is offered. Filed as
B-2026-10-24-18, and the borrow is written by hand.

After that one edit it matched the mirror on the first run under
`karac run`, the MIR interpreter, the LLVM backend, the ASan build and the
native binary, with nothing on stderr.

## Running

```bash
karac run route_plan.kara
karac build --mir route_plan.kara && ./route_plan
python3 -I -B route_plan.py > expected.out
```

About 12 s on the MIR interpreter and 0.02 s built. It writes its five
files to the working directory and removes them at the end.
