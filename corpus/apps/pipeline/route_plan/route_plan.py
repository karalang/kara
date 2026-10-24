"""Python mirror of route_plan.kara, statement for statement. Integer
arithmetic only; tdiv is Kara's `/`, which truncates toward zero.

Run: python3 -I -B route_plan.py > expected.out"""
import os

GRID = 48
STOPS = 14
INF = 1099511627776
MASK32 = 4294967295
KEY = 4096
CAPACITY = 10
UNLOAD = 4
TRUCKS = 3


def tdiv(a, b):
    q = abs(a) // abs(b)
    return q if (a < 0) == (b < 0) else -q


def tmod(a, b):
    return a - b * tdiv(a, b)


class Graph:
    def __init__(self, name, n, eu, ev, ew, start, to, wt):
        self.name, self.n, self.eu, self.ev, self.ew = name, n, eu, ev, ew
        self.start, self.to, self.wt = start, to, wt


class Stops:
    def __init__(self, node, demand):
        self.node, self.demand = node, demand


class Tour:
    def __init__(self, order, skipped, nn_len, opt_len, passes):
        self.order, self.skipped = order, skipped
        self.nn_len, self.opt_len, self.passes = nn_len, opt_len, passes


class Truck:
    def __init__(self):
        self.minutes = 0
        self.trips = []


class RouteError(Exception):
    pass


# ------------------------------------------------------------ the networks

def hash32(a):
    h = a & MASK32
    h = (((h >> 16) ^ h) * 73244475) & MASK32
    h = (((h >> 16) ^ h) * 73244475) & MASK32
    return (h >> 16) ^ h


def mix(seed, x, y, salt):
    return hash32(seed * 1000003 + y * 7919 + x * 104729 + salt * 15485863)


def build_city(name, seed):
    n = GRID * GRID
    eu, ev, ew = [], [], []
    for y in range(GRID):
        for x in range(GRID):
            u = y * GRID + x
            if x + 1 < GRID:
                h = mix(seed, x, y, 1)
                if h % 100 >= 7:
                    eu.append(u)
                    ev.append(u + 1)
                    ew.append(5 + (h // 100) % 20)
            if y + 1 < GRID:
                h = mix(seed, x, y, 2)
                if h % 100 >= 7:
                    eu.append(u)
                    ev.append(u + GRID)
                    ew.append(5 + (h // 100) % 20)
            if x + 1 < GRID and y + 1 < GRID:
                h = mix(seed, x, y, 3)
                if h % 100 < 6:
                    eu.append(u)
                    ev.append(u + GRID + 1)
                    ew.append(8 + (h // 100) % 25)
    return make_graph(name, n, eu, ev, ew)


def make_graph(name, n, eu, ev, ew):
    deg = [0] * (n + 1)
    for i in range(len(eu)):
        deg[eu[i] + 1] += 1
        deg[ev[i] + 1] += 1
    for i in range(n):
        deg[i + 1] += deg[i]
    fill = list(deg)
    to = [0] * (2 * len(eu))
    wt = [0] * (2 * len(eu))
    for i in range(len(eu)):
        a, b = eu[i], ev[i]
        to[fill[a]] = b
        wt[fill[a]] = ew[i]
        fill[a] += 1
        to[fill[b]] = a
        wt[fill[b]] = ew[i]
        fill[b] += 1
    return Graph(name, n, eu, ev, ew, deg, to, wt)


# ------------------------------------------------------------ the files

def encode_city(g):
    out = [f"city {g.name} {GRID}x{GRID} {len(g.eu)} edges\n"]
    for i in range(len(g.eu)):
        out.append(f"{g.eu[i]} {g.ev[i]} {g.ew[i]}\n")
    return "".join(out)


def text_sum(text):
    h = 2166136261
    for b in text.encode():
        h = ((h ^ b) * 16777619) & MASK32
    return h


def parse_num(field, name, line):
    s = field[1:] if field[:1] in ("-", "+") else field
    if not s or not s.isdigit() or not s.isascii():
        raise RouteError(f"{name}: line {line}: bad number '{field}'")
    return int(field)


def parse_city(name, text):
    eu, ev, ew = [], [], []
    declared = -1
    n = 0
    line_no = 0
    for line in text.splitlines():
        line_no += 1
        f = line.split(" ")
        if line_no == 1:
            if len(f) != 5 or f[0] != "city" or f[4] != "edges":
                raise RouteError(f"{name}: line 1: not a city header")
            if f[1] != name:
                raise RouteError(f"{name}: line 1: the file is for {f[1]}")
            dims = f[2].split("x")
            if len(dims) != 2:
                raise RouteError(f"{name}: line 1: bad size {f[2]}")
            w = parse_num(dims[0], name, 1)
            h = parse_num(dims[1], name, 1)
            n = w * h
            declared = parse_num(f[3], name, 1)
            continue
        if len(f) != 3:
            raise RouteError(f"{name}: line {line_no}: expected 3 fields, got {len(f)}")
        u = parse_num(f[0], name, line_no)
        v = parse_num(f[1], name, line_no)
        w = parse_num(f[2], name, line_no)
        if u < 0 or u >= n or v < 0 or v >= n:
            raise RouteError(f"{name}: line {line_no}: node out of range")
        if w <= 0:
            raise RouteError(f"{name}: line {line_no}: weight {w} is not positive")
        eu.append(u)
        ev.append(v)
        ew.append(w)
    if declared < 0:
        raise RouteError(f"{name}: empty file")
    if len(eu) != declared:
        raise RouteError(f"{name}: header says {declared} edges, found {len(eu)}")
    return make_graph(name, n, eu, ev, ew)


def same_graph(a, b):
    return a.n == b.n and a.eu == b.eu and a.ev == b.ev and a.ew == b.ew


def damage(text, at):
    out = []
    line_no = 0
    for line in text.splitlines():
        line_no += 1
        if line_no == at:
            f = line.split(" ")
            out.append(f"{f[0]} x{f[1]} {f[2]}\n")
        else:
            out.append(line)
            out.append("\n")
    return "".join(out)


def read_text(path):
    try:
        with open(path, encoding="utf-8") as fh:
            return fh.read()
    except OSError:
        raise RouteError(f"could not read {path}")


# ------------------------------------------------------------ shortest paths

def heap_push(heap, key):
    heap.append(key)
    i = len(heap) - 1
    while i > 0:
        p = tdiv(i - 1, 2)
        if heap[p] <= heap[i]:
            break
        heap[p], heap[i] = heap[i], heap[p]
        i = p


def heap_pop(heap):
    top = heap[0]
    last = heap.pop()
    n = len(heap)
    if n > 0:
        heap[0] = last
        i = 0
        while True:
            l = 2 * i + 1
            r = l + 1
            m = i
            if l < n and heap[l] < heap[m]:
                m = l
            if r < n and heap[r] < heap[m]:
                m = r
            if m == i:
                break
            heap[m], heap[i] = heap[i], heap[m]
            i = m
    return top


def dijkstra(g, src):
    dist = [INF] * g.n
    prev = [-1] * g.n
    heap = []
    dist[src] = 0
    heap_push(heap, src)
    while heap:
        key = heap_pop(heap)
        d = key // KEY
        u = key % KEY
        if d > dist[u]:
            continue
        for e in range(g.start[u], g.start[u + 1]):
            v = g.to[e]
            nd = d + g.wt[e]
            if nd < dist[v]:
                dist[v] = nd
                prev[v] = u
                heap_push(heap, nd * KEY + v)
    return dist, prev


def make_stops(g, seed):
    depot = (GRID // 2) * GRID + GRID // 2
    node = [depot]
    demand = [0]
    for k in range(STOPS):
        s = mix(seed, k, 0, 9) % g.n
        while s in node:
            s = (s + 1) % g.n
        node.append(s)
        demand.append(1 + mix(seed, k, 0, 10) % 6)
    return Stops(node, demand)


def dist_matrix(g, st):
    k = len(st.node)
    m = []
    for i in range(k):
        dist, _ = dijkstra(g, st.node[i])
        for j in range(k):
            m.append(dist[st.node[j]])
    return m


# ------------------------------------------------------------ tours

def tour_len(m, k, order):
    total = 0
    for i in range(len(order) - 1):
        total += m[order[i] * k + order[i + 1]]
    return total


def plan_tour(m, k):
    skipped = []
    left = []
    for s in range(1, k):
        if m[s] >= INF:
            skipped.append(s)
        else:
            left.append(s)
    order = [0]
    at = 0
    while left:
        best = 0
        for i in range(1, len(left)):
            if m[at * k + left[i]] < m[at * k + left[best]]:
                best = i
        at = left[best]
        order.append(at)
        del left[best]
    order.append(0)
    nn_len = tour_len(m, k, order)
    last = len(order) - 2
    passes = 0
    improved = True
    while improved:
        improved = False
        passes += 1
        for i in range(1, last):
            for j in range(i + 1, last + 1):
                a, b, c, d = order[i - 1], order[i], order[j], order[j + 1]
                delta = m[a * k + c] + m[b * k + d] - m[a * k + b] - m[c * k + d]
                if delta < 0:
                    lo, hi = i, j
                    while lo < hi:
                        order[lo], order[hi] = order[hi], order[lo]
                        lo += 1
                        hi -= 1
                    improved = True
    opt_len = tour_len(m, k, order)
    return Tour(order, skipped, nn_len, opt_len, passes)


# ------------------------------------------------------------ reports

def xy(node):
    return f"({node % GRID},{node // GRID})"


def network_report(g):
    streets = arterials = total = 0
    for i in range(len(g.eu)):
        d = g.ev[i] - g.eu[i]
        if d == 1 or d == GRID:
            streets += 1
        else:
            arterials += 1
        total += g.ew[i]
    closed = 2 * GRID * (GRID - 1) - streets
    hist = [0] * 9
    for u in range(g.n):
        hist[g.start[u + 1] - g.start[u]] += 1
    degs = " ".join(f"{d}:{hist[d]}" for d in range(9) if hist[d] > 0)
    return (f"{g.name} network: {g.n} junctions, {streets} streets ({closed} closed), "
            f"{arterials} arterials, {total} min of road, degrees {degs}")


def reach_report(g, st):
    dist, _ = dijkstra(g, st.node[0])
    reached = far = total = 0
    far_node = st.node[0]
    bands = [0] * 6
    for u in range(g.n):
        if dist[u] < INF:
            reached += 1
            total += dist[u]
            if dist[u] > far:
                far = dist[u]
                far_node = u
            b = dist[u] // 120
            if b < 5:
                bands[b] += 1
            else:
                bands[5] += 1
    mean = total // reached
    return (f"{g.name} reach: {reached}/{g.n} from the depot {xy(st.node[0])}, mean {mean} min, "
            f"farthest {xy(far_node)} at {far} min, by 2 h band "
            f"{bands[0]} {bands[1]} {bands[2]} {bands[3]} {bands[4]} {bands[5]}")


def tour_report(name, t):
    seq = " ".join("D" if s == 0 else f"s{s}" for s in t.order)
    out = (f"{name} tour: {seq}; nearest neighbour {t.nn_len} min, "
           f"2-opt {t.opt_len} min in {t.passes} passes")
    if t.skipped:
        out += "; unreachable" + "".join(f" s{x}" for x in t.skipped)
    return out


def leg_report(g, st, m, t):
    k = len(st.node)
    best = 0
    for i in range(1, len(t.order) - 1):
        if m[t.order[i] * k + t.order[i + 1]] > m[t.order[best] * k + t.order[best + 1]]:
            best = i
    frm = st.node[t.order[best]]
    to = st.node[t.order[best + 1]]
    dist, prev = dijkstra(g, frm)
    path = [to]
    at = to
    while at != frm:
        at = prev[at]
        path.append(at)
    path.reverse()
    arterial = 0
    for i in range(len(path) - 1):
        d = path[i + 1] - path[i]
        if d not in (1, -1, GRID, -GRID):
            arterial += 1
    hops = len(path) - 1
    via = ""
    for i in range(len(path)):
        if i < 3 or i >= hops - 2:
            via += f" {xy(path[i])}"
        elif i == 3:
            via += " .."
    return (f"{g.name} longest leg: {xy(frm)} to {xy(to)}, {dist[to]} min over {hops} roads "
            f"({arterial} arterial):{via}")


def demand_report(name, st, m):
    k = len(st.node)
    total = 0
    lst = ""
    for s in range(1, k):
        total += st.demand[s]
        lst += f" s{s}={st.demand[s]}"
    check = 0
    for i in range(k):
        for j in range(k):
            d = m[i * k + j]
            if d < INF:
                check = (check + d * (i + 1) * (j + 3)) % 1000000007
    return f"{name} demand: {total} units:{lst}; matrix check {check}"


# ------------------------------------------------------------ the fleet

def new_fleet():
    return [Truck() for _ in range(TRUCKS)]


def book_trip(fleet, label, stops, load, m, k):
    minutes = m[stops[0]] + m[stops[-1] * k]
    for i in range(len(stops) - 1):
        minutes += m[stops[i] * k + stops[i + 1]]
    minutes += UNLOAD * len(stops)
    pick = 0
    for i in range(1, TRUCKS):
        if fleet[i].minutes < fleet[pick].minutes:
            pick = i
    names = ",".join(f"s{s}" for s in stops)
    begin = fleet[pick].minutes
    fleet[pick].minutes += minutes
    fleet[pick].trips.append(f"{label} {names} ({load} units) {begin}-{begin + minutes}")


def dispatch(fleet, name, st, m, t):
    k = len(st.node)
    trip = []
    load = 0
    count = 0
    for i in range(1, len(t.order) - 1):
        s = t.order[i]
        if load + st.demand[s] > CAPACITY:
            count += 1
            book_trip(fleet, f"{name}#{count}", trip, load, m, k)
            trip = []
            load = 0
        trip.append(s)
        load += st.demand[s]
    if trip:
        count += 1
        book_trip(fleet, f"{name}#{count}", trip, load, m, k)


def write(path, text):
    with open(path, "w", encoding="utf-8", newline="") as fh:
        fh.write(text)


def run():
    north = build_city("north", 11)
    south = build_city("south", 23)
    east = build_city("east", 37)
    west = build_city("west", 41)

    write("route_north.txt", encode_city(north))
    write("route_south.txt", encode_city(south))
    write("route_east.txt", encode_city(east))
    write("route_west.txt", encode_city(west))

    whole = read_text("route_east.txt")
    write("route_broken.txt", damage(whole, 41))

    t1 = read_text("route_north.txt")
    t2 = read_text("route_south.txt")
    t3 = read_text("route_east.txt")
    t4 = read_text("route_west.txt")
    print(f"route_north.txt: {len(t1)} bytes, fnv {text_sum(t1):08x}")
    print(f"route_south.txt: {len(t2)} bytes, fnv {text_sum(t2):08x}")
    print(f"route_east.txt: {len(t3)} bytes, fnv {text_sum(t3):08x}")
    print(f"route_west.txt: {len(t4)} bytes, fnv {text_sum(t4):08x}")
    gn = parse_city("north", t1)
    gs = parse_city("south", t2)
    ge = parse_city("east", t3)
    gw = parse_city("west", t4)
    try:
        parse_city("east", read_text("route_broken.txt"))
        print("route_broken.txt: parsed")
    except RouteError as e:
        print(e)
    try:
        parse_city("west", t3)
        print("route_east.txt as west: parsed")
    except RouteError as e:
        print(e)
    same = same_graph(north, gn) and same_graph(south, gs) and same_graph(east, ge) and same_graph(west, gw)
    print(f"parsed networks match the built ones: {'true' if same else 'false'}")

    sn = make_stops(gn, 11)
    ss = make_stops(gs, 23)
    se = make_stops(ge, 37)
    sw = make_stops(gw, 41)

    mn = dist_matrix(gn, sn)
    ms = dist_matrix(gs, ss)
    me = dist_matrix(ge, se)
    mw = dist_matrix(gw, sw)

    k = STOPS + 1
    tn = plan_tour(mn, k)
    ts = plan_tour(ms, k)
    te = plan_tour(me, k)
    tw = plan_tour(mw, k)

    lines = [
        network_report(gn), network_report(gs), network_report(ge), network_report(gw),
        reach_report(gn, sn), reach_report(gs, ss), reach_report(ge, se), reach_report(gw, sw),
        tour_report("north", tn), tour_report("south", ts), tour_report("east", te), tour_report("west", tw),
        leg_report(gn, sn, mn, tn), leg_report(gs, ss, ms, ts), leg_report(ge, se, me, te), leg_report(gw, sw, mw, tw),
        demand_report("north", sn, mn), demand_report("south", ss, ms),
        demand_report("east", se, me), demand_report("west", sw, mw),
    ]
    for line in lines:
        print(line)

    fleet = new_fleet()
    dispatch(fleet, "north", sn, mn, tn)
    dispatch(fleet, "south", ss, ms, ts)
    dispatch(fleet, "east", se, me, te)
    dispatch(fleet, "west", sw, mw, tw)
    makespan = 0
    busy = 0
    for i in range(TRUCKS):
        t = fleet[i]
        print(f"truck {i + 1}: {len(t.trips)} trips, {t.minutes} min")
        for trip in t.trips:
            print(f"  {trip}")
        busy += t.minutes
        if t.minutes > makespan:
            makespan = t.minutes
    print(f"fleet: {busy} truck-minutes, done after {makespan} min")

    for p in ["route_north.txt", "route_south.txt", "route_east.txt", "route_west.txt", "route_broken.txt"]:
        os.remove(p)


def main():
    try:
        run()
    except RouteError as e:
        print(f"route_plan failed: {e}")


main()
