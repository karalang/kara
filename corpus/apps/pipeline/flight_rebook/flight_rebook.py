"""The Python mirror of flight_rebook.kara: an airline's day of irregular
operations at four hubs. It writes expected.out.

Run: python3 -I -B flight_rebook.py > expected.out
     FLIGHT_REVERSE=1 python3 -I -B flight_rebook.py   # the hubs ask for spares in reverse"""
import os
import sys

MASK32 = 4294967295
MCT = 45
LOC_ALPHA = "ABCDEFGHJKLMNPQRSTUVWXYZ23456789"
CITIES = ["AUS", "BNA", "BOS", "CLT", "DCA", "DFW", "DTW", "IAH", "LAS", "LAX", "LGA", "MCI",
          "MCO", "MIA", "MSP", "PDX", "PHX", "RDU", "SAN", "SEA", "SFO", "SLC", "STL", "TPA"]
N_CITIES = 10
# aircraft, business seats, economy seats
AIRCRAFT = [("E175", 12, 64), ("A220", 12, 118), ("A320", 12, 138), ("737-800", 16, 150), ("A321", 20, 170)]
TIERS = ["-", "SIL", "GLD", "PLT"]
REASONS = ["WX", "ATC", "MX", "CREW"]
CAUSES = ["cancelled", "inbound cancelled", "missed connection"]
MEAL = 1500
MEAL_LONG = 3000
HOTEL = 13900
CREDIT_LO = [20000, 10000]
CREDIT_HI = [40000, 25000]
MILES = [0, 0, 5000, 10000]
SPARES = ["N801NS", "N802NS", "N803NS", "N804NS"]
SPARE_SEATS = 150
MIN_SECTION = 25
MAX_PER_HUB = 2


class Hub:
    def __init__(self, code, base, seed, ws, we, cnx, dmin, dmax, bg, mx, reason):
        self.code = code
        self.base = base
        self.seed = seed
        self.ws = ws
        self.we = we
        self.cnx = cnx
        self.dmin = dmin
        self.dmax = dmax
        self.bg = bg
        self.mx = mx
        self.reason = reason


class Flight:
    def __init__(self, fid, orig, dest, dep, arr, ac, sj, sy, oj, oy):
        self.fid = fid
        self.orig = orig
        self.dest = dest
        self.dep = dep
        self.arr = arr
        self.ac = ac
        self.seats = [sj, sy]
        self.open = [oj, oy]

    def key(self):
        return (self.fid, self.orig, self.dest, self.dep, self.arr, self.ac, tuple(self.seats), tuple(self.open))


class Pnr:
    def __init__(self, loc, party, cabin, fare, tier, amount, inb, outb):
        self.loc = loc
        self.party = party
        self.cabin = cabin
        self.fare = fare
        self.tier = tier
        self.amount = amount
        self.inb = inb
        self.outb = outb

    def key(self):
        return (self.loc, self.party, self.cabin, self.fare, self.tier, self.amount, self.inb, self.outb)


class Event:
    def __init__(self, fid, kind, minutes, reason):
        self.fid = fid
        self.kind = kind
        self.minutes = minutes
        self.reason = reason

    def key(self):
        return (self.fid, self.kind, self.minutes, self.reason)


class BatchError(Exception):
    pass


# ------------------------------------------------------------ helpers

def hash32(a):
    h = a & MASK32
    h = (((h >> 16) ^ h) * 73244475) & MASK32
    h = (((h >> 16) ^ h) * 73244475) & MASK32
    return (h >> 16) ^ h


def mix(seed, i, salt):
    return hash32(seed * 1000003 + i * 104729 + salt * 15485863)


def text_sum(text):
    h = 2166136261
    for b in text.encode():
        h = ((h ^ b) * 16777619) & MASK32
    return h


def group3(n):
    s = str(n)
    out = ""
    while len(s) > 3:
        out = "," + s[len(s) - 3:] + out
        s = s[:len(s) - 3]
    return s + out


def money(c):
    sign = "-" if c < 0 else ""
    a = abs(c)
    return "%s$%s.%02d" % (sign, group3(a // 100), a % 100)


def dollars(c):
    return "%d.%02d" % (c // 100, c % 100)


def is_digits(s):
    return len(s) > 0 and all("0" <= ch <= "9" for ch in s)


def cents_of(t):
    dot = t.find(".")
    if dot < 1 or len(t) - dot != 3:
        return None
    a = t[:dot]
    b = t[dot + 1:]
    if not (is_digits(a) and is_digits(b)) or len(a) > 7:
        return None
    return int(a) * 100 + int(b)


def small_int(t, lo, hi):
    if not is_digits(t) or len(t) > 6:
        return None
    v = int(t)
    if v < lo or v > hi:
        return None
    return v


def hhmm(t):
    return "%02d:%02d" % (t // 60, t % 60)


def clock(t):
    if t >= 1440:
        return hhmm(t - 1440) + "+1"
    return hhmm(t)


def time_of(t):
    if len(t) != 5 or t[2] != ":":
        return None
    a = t[:2]
    b = t[3:]
    if not (is_digits(a) and is_digits(b)):
        return None
    h = int(a)
    m = int(b)
    if h > 23 or m > 59:
        return None
    return h * 60 + m


def pct(a, b):
    if b == 0:
        return 0
    return a * 100 // b


def tenths(a, b):
    v = a * 1000 // b
    return "%d.%d" % (v // 10, v % 10)


def city_ok(t):
    return len(t) == 3 and all("A" <= ch <= "Z" for ch in t)


def index_in(xs, t):
    for i in range(len(xs)):
        if xs[i] == t:
            return i
    return -1


def file_line(name, text):
    rows = len(text.split("\n")) - 2
    return "%s: %s rows, %s bytes, fnv %08x" % (name, group3(rows), group3(len(text)), text_sum(text))


# ------------------------------------------------------------ building

def cities_of(hub):
    order = sorted(range(len(CITIES)), key=lambda i: (mix(hub.seed, i, 1), i))
    return sorted(CITIES[i] for i in order[:N_CITIES])


def block_of(hub, k):
    return (55 + mix(hub.seed, k, 2) % 276) // 5 * 5


def aircraft_of(hub, block, num):
    r = mix(hub.seed, num, 3)
    if block < 100:
        return r % 2
    if block < 200:
        return 1 + r % 3
    return 2 + r % 3


def build_flights(hub):
    out = []
    cities = cities_of(hub)
    for k in range(N_CITIES):
        city = cities[k]
        block = block_of(hub, k)
        freq = 3 + mix(hub.seed, k, 4) % 2
        span_end = min(1260, 1380 - block)
        step = (span_end - 360) // freq // 5 * 5
        start = max(390, block + 300)
        step2 = (1380 - start) // freq // 5 * 5
        for j in range(freq):
            num = hub.base + k * 10 + j * 2
            dep = 360 + j * step + (mix(hub.seed, k * 8 + j, 5) % (step // 2 + 1)) // 5 * 5
            a = aircraft_of(hub, block, num)
            ac = AIRCRAFT[a]
            out.append(Flight("NS%d" % num, hub.code, city, dep, dep + block, ac[0], ac[1], ac[2],
                              mix(hub.seed, num, 7) % 3, 3 + mix(hub.seed, num, 8) % 16))
            arr = start + j * step2 + (mix(hub.seed, k * 8 + j, 6) % (step2 // 2 + 1)) // 5 * 5
            a2 = aircraft_of(hub, block, num + 1)
            ac2 = AIRCRAFT[a2]
            out.append(Flight("NS%d" % (num + 1), city, hub.code, arr - block, arr, ac2[0], ac2[1], ac2[2],
                              mix(hub.seed, num + 1, 7) % 3, 3 + mix(hub.seed, num + 1, 8) % 16))
    return out


def party_of(r):
    x = r % 100
    if x < 52:
        return 1
    if x < 80:
        return 2
    if x < 92:
        return 3
    return 4


def locator(hub_index, n):
    x = ((hub_index * 200000 + n) * 387420489 + 12345) & 1073741823
    s = ""
    for _ in range(6):
        s = LOC_ALPHA[x % 32] + s
        x //= 32
    return s


def make_pnr(hub_index, hub, n, r, party, cabin, blocks, inb, outb):
    t = (r // 7) % 100
    if cabin == 0:
        t += 15
    tier = 0
    if t >= 94:
        tier = 3
    elif t >= 82:
        tier = 2
    elif t >= 60:
        tier = 1
    fare = "F"
    if cabin == 1 and (r // 13) % 10 < 3:
        fare = "B"
    y = 4900 + blocks * 23 + (r // 17) % 6000
    if fare == "B":
        y = y * 70 // 100
    if cabin == 0:
        y = y * 35 // 10
    return Pnr(locator(hub_index, n), party, "JY"[cabin], fare, TIERS[tier], y * party, inb, outb)


def build_pnrs(hub_index, hub, flights):
    nf = len(flights)
    left = []
    for f in range(nf):
        fl = flights[f]
        lj = fl.seats[0] * (60 + mix(hub.seed, f, 9) % 41) // 100
        ly = fl.seats[1] * (78 + mix(hub.seed, f, 10) % 21) // 100
        left.append([lj, ly])
    out = []
    n = 0
    for o in range(nf):
        fo = flights[o]
        if fo.orig != hub.code:
            continue
        goal = (left[o][0] + left[o][1]) * 45 // 100
        placed = 0
        attempt = 0
        while placed < goal and attempt < 60:
            r = mix(hub.seed, o * 1000 + attempt, 11)
            attempt += 1
            cabin = 0 if (r // 100) % 9 == 0 else 1
            party = min(party_of(r), left[o][cabin])
            if party == 0:
                continue
            cands = []
            for i in range(nf):
                fi = flights[i]
                if fi.dest == hub.code and fi.orig != fo.dest and fi.arr + MCT <= fo.dep and fi.arr >= fo.dep - 240 and left[i][cabin] >= party:
                    cands.append(i)
            if len(cands) == 0:
                continue
            i = cands[(r // 1000) % len(cands)]
            out.append(make_pnr(hub_index, hub, n, r, party, cabin, block_of_pair(flights, i, o), flights[i].fid, fo.fid))
            n += 1
            left[o][cabin] -= party
            left[i][cabin] -= party
            placed += party
        for cabin in range(2):
            while left[o][cabin] > 0:
                r = mix(hub.seed, o * 1000 + attempt, 12)
                attempt += 1
                party = min(party_of(r), left[o][cabin])
                out.append(make_pnr(hub_index, hub, n, r, party, cabin, fo.arr - fo.dep, "-", fo.fid))
                n += 1
                left[o][cabin] -= party
    for i in range(nf):
        fi = flights[i]
        if fi.dest != hub.code:
            continue
        attempt = 0
        for cabin in range(2):
            while left[i][cabin] > 0:
                r = mix(hub.seed, i * 1000 + attempt, 13)
                attempt += 1
                party = min(party_of(r), left[i][cabin])
                out.append(make_pnr(hub_index, hub, n, r, party, cabin, fi.arr - fi.dep, fi.fid, "-"))
                n += 1
                left[i][cabin] -= party
    return out


def block_of_pair(flights, i, o):
    return flights[i].arr - flights[i].dep + flights[o].arr - flights[o].dep


def build_events(hub, flights):
    out = []
    for f in range(len(flights)):
        fl = flights[f]
        at = fl.dep if fl.orig == hub.code else fl.arr
        r = mix(hub.seed, f, 14)
        if hub.ws <= at <= hub.we:
            if r % 100 < hub.cnx:
                out.append(Event(fl.fid, "CNX", 0, hub.reason))
            else:
                d = hub.dmin + (r // 100) % (hub.dmax - hub.dmin + 1)
                out.append(Event(fl.fid, "DLY", d // 5 * 5, hub.reason))
        elif r % 1000 < hub.mx:
            out.append(Event(fl.fid, "CNX", 0, "MX"))
        elif fl.dest == hub.code and (r // 1000) % 100 < 3:
            out.append(Event(fl.fid, "CNX", 0, "CREW"))
        elif (r // 7) % 100 < hub.bg:
            d = 10 + (r // 701) % 51
            out.append(Event(fl.fid, "DLY", d // 5 * 5, REASONS[1 + (r // 31) % 3]))
    return out


def flight_header():
    return "flight,origin,dest,dep,arr,aircraft,seats_j,seats_y,open_j,open_y"


def pnr_header():
    return "pnr,party,cabin,fare,tier,amount,inbound,outbound"


def event_header():
    return "flight,event,minutes,reason"


def encode_flights(flights):
    out = flight_header() + "\n"
    for f in flights:
        out += "%s,%s,%s,%s,%s,%s,%d,%d,%d,%d\n" % (f.fid, f.orig, f.dest, hhmm(f.dep), hhmm(f.arr), f.ac,
                                                  f.seats[0], f.seats[1], f.open[0], f.open[1])
    return out


def encode_pnrs(pnrs):
    out = pnr_header() + "\n"
    for p in pnrs:
        out += "%s,%d,%s,%s,%s,%s,%s,%s\n" % (p.loc, p.party, p.cabin, p.fare, p.tier, dollars(p.amount), p.inb, p.outb)
    return out


def encode_events(events):
    out = event_header() + "\n"
    for e in events:
        out += "%s,%s,%d,%s\n" % (e.fid, e.kind, e.minutes, e.reason)
    return out


def read_text(path):
    try:
        with open(path) as f:
            return f.read()
    except OSError:
        raise BatchError("cannot read %s" % path)


def write_text(path, text):
    with open(path, "w") as f:
        f.write(text)


def damage(text, n, field, value):
    lines = text.split("\n")
    f = lines[n].split(",")
    f[field] = value
    lines[n] = ",".join(f)
    return "\n".join(lines)


# ------------------------------------------------------------ parsing

def body_lines(path, text, header):
    lines = text.split("\n")
    n = len(lines)
    if n < 2 or lines[n - 1] != "":
        raise BatchError("%s: no final newline" % path)
    if lines[0] != header:
        raise BatchError("%s line 1: bad header" % path)
    return lines[1:n - 1]


def fields_of(path, n, line, want):
    f = line.split(",")
    if len(f) != want:
        raise BatchError("%s line %d: expected %d fields, found %d" % (path, n, want, len(f)))
    return f


def int_field(path, n, what, t, lo, hi):
    v = small_int(t, lo, hi)
    if v is None:
        raise BatchError("%s line %d: bad %s '%s'" % (path, n, what, t))
    return v


def time_field(path, n, what, t):
    v = time_of(t)
    if v is None:
        raise BatchError("%s line %d: bad %s '%s'" % (path, n, what, t))
    return v


def flight_id_ok(t):
    return len(t) == 6 and t[:2] == "NS" and is_digits(t[2:])


def find_flight(flights, fid):
    for i in range(len(flights)):
        if flights[i].fid == fid:
            return i
    return -1


def parse_flights(path, hub, text):
    body = body_lines(path, text, flight_header())
    out = []
    for k in range(len(body)):
        n = k + 2
        f = fields_of(path, n, body[k], 10)
        if not flight_id_ok(f[0]):
            raise BatchError("%s line %d: bad flight '%s'" % (path, n, f[0]))
        if find_flight(out, f[0]) >= 0:
            raise BatchError("%s line %d: flight %s appears twice" % (path, n, f[0]))
        if not city_ok(f[1]) or not city_ok(f[2]) or f[1] == f[2]:
            raise BatchError("%s line %d: bad route '%s-%s'" % (path, n, f[1], f[2]))
        if f[1] != hub.code and f[2] != hub.code:
            raise BatchError("%s line %d: flight %s does not touch %s" % (path, n, f[0], hub.code))
        dep = time_field(path, n, "departure", f[3])
        arr = time_field(path, n, "arrival", f[4])
        if arr <= dep:
            raise BatchError("%s line %d: flight %s arrives before it leaves" % (path, n, f[0]))
        ok = False
        for ac in AIRCRAFT:
            if ac[0] == f[5]:
                ok = True
        if not ok:
            raise BatchError("%s line %d: bad aircraft '%s'" % (path, n, f[5]))
        sj = int_field(path, n, "business seats", f[6], 0, 400)
        sy = int_field(path, n, "economy seats", f[7], 1, 400)
        oj = int_field(path, n, "open business seats", f[8], 0, sj)
        oy = int_field(path, n, "open economy seats", f[9], 0, sy)
        out.append(Flight(f[0], f[1], f[2], dep, arr, f[5], sj, sy, oj, oy))
    return out


def loc_ok(t):
    if len(t) != 6:
        return False
    for ch in t:
        if ch not in LOC_ALPHA:
            return False
    return True


def parse_pnrs(path, hub, flights, text):
    body = body_lines(path, text, pnr_header())
    out = []
    seen = set()
    booked = [[0, 0] for _ in flights]
    for k in range(len(body)):
        n = k + 2
        f = fields_of(path, n, body[k], 8)
        if not loc_ok(f[0]):
            raise BatchError("%s line %d: bad record locator '%s'" % (path, n, f[0]))
        if f[0] in seen:
            raise BatchError("%s line %d: record locator %s appears twice" % (path, n, f[0]))
        seen.add(f[0])
        party = int_field(path, n, "party", f[1], 1, 4)
        cabin = index_in(["J", "Y"], f[2])
        if cabin < 0:
            raise BatchError("%s line %d: bad cabin '%s'" % (path, n, f[2]))
        if f[3] != "F" and f[3] != "B":
            raise BatchError("%s line %d: bad fare '%s'" % (path, n, f[3]))
        if f[3] == "B" and cabin == 0:
            raise BatchError("%s line %d: a basic fare in business" % (path, n))
        if index_in(TIERS, f[4]) < 0:
            raise BatchError("%s line %d: bad tier '%s'" % (path, n, f[4]))
        amount = cents_of(f[5])
        if amount is None:
            raise BatchError("%s line %d: bad amount '%s'" % (path, n, f[5]))
        legs = []
        for side in range(2):
            t = f[6 + side]
            if t == "-":
                legs.append(-1)
                continue
            i = find_flight(flights, t)
            if i < 0:
                raise BatchError("%s line %d: flight %s is not in the schedule" % (path, n, t))
            if side == 0 and flights[i].dest != hub.code:
                raise BatchError("%s line %d: flight %s does not arrive at %s" % (path, n, t, hub.code))
            if side == 1 and flights[i].orig != hub.code:
                raise BatchError("%s line %d: flight %s does not leave %s" % (path, n, t, hub.code))
            legs.append(i)
        if legs[0] < 0 and legs[1] < 0:
            raise BatchError("%s line %d: no flights" % (path, n))
        if legs[0] >= 0 and legs[1] >= 0:
            a = flights[legs[0]]
            b = flights[legs[1]]
            if a.orig == b.dest:
                raise BatchError("%s line %d: %s and %s go back where they started" % (path, n, a.fid, b.fid))
            if a.arr + MCT > b.dep:
                raise BatchError("%s line %d: the connection from %s to %s is under %d minutes" % (path, n, a.fid, b.fid, MCT))
        for i in legs:
            if i >= 0:
                booked[i][cabin] += party
                if booked[i][cabin] > flights[i].seats[cabin]:
                    raise BatchError("%s line %d: flight %s is oversold in %s" % (path, n, flights[i].fid, ["business", "economy"][cabin]))
        out.append(Pnr(f[0], party, f[2], f[3], f[4], amount, f[6], f[7]))
    return out


def parse_events(path, flights, text):
    body = body_lines(path, text, event_header())
    out = []
    last = ""
    for k in range(len(body)):
        n = k + 2
        f = fields_of(path, n, body[k], 4)
        if find_flight(flights, f[0]) < 0:
            raise BatchError("%s line %d: flight %s is not in the schedule" % (path, n, f[0]))
        if f[0] <= last:
            raise BatchError("%s line %d: flight %s is out of order" % (path, n, f[0]))
        last = f[0]
        if f[1] != "CNX" and f[1] != "DLY":
            raise BatchError("%s line %d: bad event '%s'" % (path, n, f[1]))
        m = int_field(path, n, "minutes", f[2], 0, 720)
        if f[1] == "CNX" and m != 0:
            raise BatchError("%s line %d: a cancellation with a delay" % (path, n))
        if f[1] == "DLY" and m == 0:
            raise BatchError("%s line %d: a delay of no minutes" % (path, n))
        if index_in(REASONS, f[3]) < 0:
            raise BatchError("%s line %d: bad reason '%s'" % (path, n, f[3]))
        out.append(Event(f[0], f[1], m, f[3]))
    return out


# ------------------------------------------------------------ rebooking

class Day:
    """What the day did to a hub's flights and passengers."""

    def __init__(self, flights):
        nf = len(flights)
        self.cnx = [False] * nf
        self.delay = [0] * nf
        self.reason = [""] * nf
        self.seats = [[[0, 0] for _ in range(nf)], [[fl.open[0], fl.open[1]] for fl in flights]]
        self.leg_in = []
        self.leg_out = []
        self.cause = []
        # 0 not disrupted, 1 same day, 2 next day, 3 stranded
        self.outcome = []
        self.new_in = []
        self.in_day = []
        self.new_out = []
        self.out_day = []
        self.down = []
        self.kept = []
        self.arrive = []
        self.first_dep = []
        self.order = []


def eff_dep(day, flights, f, d):
    if d == 0:
        return flights[f].dep + day.delay[f]
    return 1440 + flights[f].dep


def eff_arr(day, flights, f, d):
    if d == 0:
        return flights[f].arr + day.delay[f]
    return 1440 + flights[f].arr


def search(day, flights, cands, ready, party, cabin):
    """The earliest arrival among cands that leaves at or after ready with
    room for the party, today in the booked cabin, today in economy,
    tomorrow in the booked cabin, tomorrow in economy: (flight, day,
    cabin), or flight -1."""
    for d in range(2):
        tries = [1] if cabin == 1 else [0, 1]
        for cb in tries:
            best = -1
            best_arr = 0
            for f in cands:
                if d == 0 and day.cnx[f]:
                    continue
                if eff_dep(day, flights, f, d) < ready:
                    continue
                if day.seats[d][f][cb] < party:
                    continue
                a = eff_arr(day, flights, f, d)
                if best < 0 or a < best_arr:
                    best = f
                    best_arr = a
            if best >= 0:
                return (best, d, cb)
    return (-1, 0, 0)


def rebook(hub, flights, pnrs, events):
    nf = len(flights)
    day = Day(flights)
    for e in events:
        f = find_flight(flights, e.fid)
        day.reason[f] = e.reason
        if e.kind == "CNX":
            day.cnx[f] = True
        else:
            day.delay[f] = e.minutes
    for f in range(nf):
        day.seats[0][f] = [flights[f].seats[0], flights[f].seats[1]]
    to_city = {}
    from_city = {}
    for f in range(nf):
        if flights[f].orig == hub.code:
            to_city.setdefault(flights[f].dest, []).append(f)
        else:
            from_city.setdefault(flights[f].orig, []).append(f)
    np_ = len(pnrs)
    for p in pnrs:
        cabin = 0 if p.cabin == "J" else 1
        li = find_flight(flights, p.inb) if p.inb != "-" else -1
        lo = find_flight(flights, p.outb) if p.outb != "-" else -1
        day.leg_in.append(li)
        day.leg_out.append(lo)
        for i in (li, lo):
            if i >= 0:
                day.seats[0][i][cabin] -= p.party
        cause = -1
        if li >= 0 and day.cnx[li]:
            cause = 1
        elif lo >= 0 and day.cnx[lo]:
            cause = 0
        elif li >= 0 and lo >= 0 and eff_arr(day, flights, li, 0) + MCT > eff_dep(day, flights, lo, 0):
            cause = 2
        day.cause.append(cause)
        day.outcome.append(0)
        day.new_in.append(li)
        day.in_day.append(0)
        day.new_out.append(lo)
        day.out_day.append(0)
        day.down.append(False)
        day.kept.append(False)
        first = li if li >= 0 else lo
        day.first_dep.append(eff_dep(day, flights, first, 0))
        last = lo if lo >= 0 else li
        day.arrive.append(eff_arr(day, flights, last, 0))
    order = [j for j in range(np_) if day.cause[j] >= 0]
    order.sort(key=lambda j: (-index_in(TIERS, pnrs[j].tier), 0 if pnrs[j].cabin == "J" else 1,
                              0 if pnrs[j].fare == "F" else 1, j))
    day.order = order
    for j in order:
        p = pnrs[j]
        cabin = 0 if p.cabin == "J" else 1
        li = day.leg_in[j]
        lo = day.leg_out[j]
        ready = 0
        ok = True
        first = -1
        if li >= 0:
            if day.cnx[li]:
                got = search(day, flights, from_city[flights[li].orig], flights[li].dep, p.party, cabin)
                if got[0] < 0:
                    ok = False
                else:
                    day.new_in[j] = got[0]
                    day.in_day[j] = got[1]
                    day.seats[got[1]][got[0]][got[2]] -= p.party
                    if got[2] != cabin:
                        day.down[j] = True
                    ready = eff_arr(day, flights, got[0], got[1]) + MCT
                    first = eff_dep(day, flights, got[0], got[1])
                    if lo < 0:
                        day.arrive[j] = eff_arr(day, flights, got[0], got[1])
            else:
                ready = eff_arr(day, flights, li, 0) + MCT
                first = eff_dep(day, flights, li, 0)
        else:
            ready = flights[lo].dep
        if ok and lo >= 0:
            if not day.cnx[lo] and eff_dep(day, flights, lo, 0) >= ready:
                day.kept[j] = True
                if first < 0:
                    first = eff_dep(day, flights, lo, 0)
            else:
                if not day.cnx[lo]:
                    day.seats[0][lo][cabin] += p.party
                got = search(day, flights, to_city[flights[lo].dest], ready, p.party, cabin)
                if got[0] < 0:
                    ok = False
                    if li >= 0 and day.cnx[li]:
                        day.seats[day.in_day[j]][day.new_in[j]][1 if day.down[j] else cabin] += p.party
                        day.down[j] = False
                else:
                    day.new_out[j] = got[0]
                    day.out_day[j] = got[1]
                    day.seats[got[1]][got[0]][got[2]] -= p.party
                    if got[2] != cabin:
                        day.down[j] = True
                    day.arrive[j] = eff_arr(day, flights, got[0], got[1])
                    if first < 0:
                        first = eff_dep(day, flights, got[0], got[1])
        if not ok:
            day.outcome[j] = 3
            day.new_in[j] = -1 if li >= 0 and day.cnx[li] else li
            day.new_out[j] = -1
            continue
        day.first_dep[j] = first
        if day.in_day[j] == 1 or day.out_day[j] == 1:
            day.outcome[j] = 2
        else:
            day.outcome[j] = 1
    return day


def encode_rebook(flights, pnrs, day):
    out = "pnr,cause,outcome,inbound,outbound,cabin,arrival\n"
    names = ["", "same day", "next day", "stranded"]
    for j in day.order:
        p = pnrs[j]
        legs = []
        for (f, d, orig) in ((day.new_in[j], day.in_day[j], day.leg_in[j]), (day.new_out[j], day.out_day[j], day.leg_out[j])):
            if orig < 0:
                legs.append("-")
            elif f < 0:
                legs.append("none")
            else:
                legs.append(flights[f].fid + ("+1" if d == 1 else ""))
        cabin = "Y" if day.down[j] else p.cabin
        arr = "-" if day.outcome[j] == 3 else clock(day.arrive[j])
        out += "%s,%s,%s,%s,%s,%s,%s\n" % (p.loc, CAUSES[day.cause[j]], names[day.outcome[j]], legs[0], legs[1], cabin, arr)
    return out


# ------------------------------------------------------------ reports

def ops_report(hub, flights, day):
    n = len(flights)
    c = 0
    by = [0, 0, 0, 0]
    d = 0
    total = 0
    worst = -1
    for f in range(n):
        if day.cnx[f]:
            c += 1
            by[index_in(REASONS, day.reason[f])] += 1
        elif day.delay[f] > 0:
            d += 1
            total += day.delay[f]
            if worst < 0 or day.delay[f] > day.delay[worst]:
                worst = f
    parts = []
    for r in range(4):
        if by[r] > 0:
            parts.append("%s %d" % (REASONS[r], by[r]))
    s = "%s ops: %d flights, %d cancelled" % (hub.code, n, c)
    if c > 0:
        s += " (%s)" % ", ".join(parts)
    s += ", %d delayed, %d min average delay" % (d, total // max(d, 1))
    if worst >= 0:
        fl = flights[worst]
        s += ", worst %s %s-%s %s +%d" % (fl.fid, fl.orig, fl.dest, hhmm(fl.dep), day.delay[worst])
    return s


def rebook_report(hub, pnrs, day):
    lines = []
    n = [0, 0, 0]
    pax = 0
    for j in day.order:
        n[day.cause[j]] += 1
        pax += pnrs[j].party
    lines.append("%s disrupted: %s PNRs, %s passengers: %d on cancelled flights, %d with a cancelled inbound, %d missed connections"
                 % (hub.code, group3(len(day.order)), group3(pax), n[0], n[1], n[2]))
    out = [0, 0, 0, 0]
    kept = 0
    down = 0
    for j in day.order:
        out[day.outcome[j]] += pnrs[j].party
        if day.kept[j]:
            kept += pnrs[j].party
        if day.down[j]:
            down += pnrs[j].party
    lines.append("  passengers: %s same day (%d kept their outbound, %d downgraded), %s next day, %s stranded"
                 % (group3(out[1]), kept, down, group3(out[2]), group3(out[3])))
    for t in range(3, -1, -1):
        tot = 0
        o = [0, 0, 0, 0]
        for j in day.order:
            if pnrs[j].tier == TIERS[t]:
                tot += pnrs[j].party
                o[day.outcome[j]] += pnrs[j].party
        if tot > 0:
            name = "no tier" if t == 0 else TIERS[t]
            lines.append("  %s: %d passengers, %d%% same day, %d%% next day, %d%% stranded"
                         % (name, tot, pct(o[1], tot), pct(o[2], tot), pct(o[3], tot)))
    return lines


def load_report(hub, flights, day):
    cap = 0
    booked = 0
    flown = 0
    full = 0
    low = -1
    low_b = 0
    for f in range(len(flights)):
        if day.cnx[f]:
            continue
        flown += 1
        c = flights[f].seats[0] + flights[f].seats[1]
        b = c - day.seats[0][f][0] - day.seats[0][f][1]
        cap += c
        booked += b
        if b == c:
            full += 1
        if low < 0 or b * (flights[low].seats[0] + flights[low].seats[1]) < low_b * c:
            low = f
            low_b = b
    open_ = 0
    taken = 0
    for f in range(len(flights)):
        o = flights[f].open[0] + flights[f].open[1]
        open_ += o
        taken += o - day.seats[1][f][0] - day.seats[1][f][1]
    fl = flights[low]
    return ("%s loads: %s%% of %s seats on %d flights flown, %d full, emptiest %s %s-%s %d/%d; tomorrow %d of %d open seats taken"
            % (hub.code, tenths(booked, cap), group3(cap), flown, full, fl.fid, fl.orig, fl.dest, low_b,
               fl.seats[0] + fl.seats[1], taken, open_))


def care_report(hub, flights, pnrs, day):
    meals = 0
    meal_n = 0
    rooms = 0
    credits = 0
    refunds = 0
    downs = 0
    miles = 0
    for j in range(len(pnrs)):
        p = pnrs[j]
        cabin = 0 if p.cabin == "J" else 1
        oc = day.outcome[j]
        li = day.leg_in[j]
        lo = day.leg_out[j]
        if day.cause[j] >= 0:
            miles += MILES[index_in(TIERS, p.tier)] * p.party
        if oc == 3:
            if p.fare == "F":
                refunds += p.amount
            else:
                credits += p.amount
            continue
        first = li if li >= 0 else lo
        wait = day.first_dep[j] - flights[first].dep
        if wait >= 300:
            meals += MEAL_LONG * p.party
            meal_n += p.party
        elif wait >= 120:
            meals += MEAL * p.party
            meal_n += p.party
        if lo >= 0 and day.out_day[j] == 1 and day.in_day[j] == 0 and li >= 0:
            rooms += (p.party + 1) // 2
        last = lo if lo >= 0 else li
        late = day.arrive[j] - flights[last].arr
        if oc == 2 or late >= 360:
            credits += CREDIT_HI[cabin] * p.party
        elif late >= 180:
            credits += CREDIT_LO[cabin] * p.party
        if day.down[j]:
            downs += p.amount * 40 // 100
    return ("%s care: meals %s (%s vouchers), hotels %s (%d rooms), credits %s, refunds %s, downgrade refunds %s, bonus miles %s"
            % (hub.code, money(meals), group3(meal_n), money(rooms * HOTEL), rooms, money(credits), money(refunds), money(downs), group3(miles)))


# ------------------------------------------------------------ spares

class Pool:
    def __init__(self):
        self.left = len(SPARES)
        self.log = []


def ask_spares(pool, hub, flights, pnrs, day):
    """The ops center's spare aircraft: a hub's destinations with the most
    stranded passengers each get an extra section tomorrow at 07:00, up to
    MAX_PER_HUB, while spares last."""
    by = {}
    for j in day.order:
        if day.outcome[j] == 3 and day.leg_out[j] >= 0:
            d = flights[day.leg_out[j]].dest
            by[d] = by.get(d, 0) + pnrs[j].party
    want = sorted([d for d in by if by[d] >= MIN_SECTION], key=lambda d: (-by[d], d))
    if len(want) == 0:
        return ["%s spares: no destination has %d stranded passengers" % (hub.code, MIN_SECTION)]
    lines = []
    unserved = []
    for d in want[:MAX_PER_HUB]:
        if pool.left == 0:
            unserved.append("%s (%d stranded)" % (d, by[d]))
            continue
        tail = SPARES[len(SPARES) - pool.left]
        pool.left -= 1
        seats = SPARE_SEATS
        boarded = 0
        saved = 0
        for j in day.order:
            p = pnrs[j]
            if day.outcome[j] != 3 or day.leg_out[j] < 0 or flights[day.leg_out[j]].dest != d:
                continue
            if p.party > seats:
                continue
            seats -= p.party
            boarded += p.party
            if p.fare == "F":
                saved += p.amount
            pool.log.append("%s,%s,%s,%s,%d" % (tail, hub.code, d, p.loc, p.party))
        lines.append("%s spares: %s to %s at 07:00 tomorrow, %d of %d stranded passengers, %s in refunds saved"
                     % (hub.code, tail, d, boarded, by[d], money(saved)))
    if len(unserved) > 0:
        lines.append("%s spares: none left for %s" % (hub.code, ", ".join(unserved)))
    return lines


# ------------------------------------------------------------ the job

def run():
    hubs = [
        Hub("ORD", 1000, 7101, 840, 1080, 35, 20, 150, 18, 6, "WX"),
        Hub("DEN", 2000, 7202, 360, 630, 45, 20, 120, 15, 6, "WX"),
        Hub("ATL", 3000, 7303, 660, 1200, 12, 45, 210, 20, 6, "ATC"),
        Hub("JFK", 4000, 7404, 960, 1290, 25, 20, 180, 22, 30, "WX"),
    ]
    sched = [build_flights(h) for h in hubs]
    books = [build_pnrs(i, hubs[i], sched[i]) for i in range(4)]
    evs = [build_events(hubs[i], sched[i]) for i in range(4)]
    names = []
    for h in hubs:
        names += [h.code + "_flights.csv", h.code + "_pnrs.csv", h.code + "_events.csv"]
    for i in range(4):
        write_text(names[i * 3], encode_flights(sched[i]))
        write_text(names[i * 3 + 1], encode_pnrs(books[i]))
        write_text(names[i * 3 + 2], encode_events(evs[i]))
    whole = read_text("ORD_pnrs.csv")
    write_text("ORD_pnrs.bad", damage(whole, 300, 1, "5"))
    whole2 = read_text("ATL_events.csv")
    write_text("ATL_events.bad", damage(whole2, 5, 1, "DLA"))
    texts = [read_text(p) for p in names]
    labels = []
    for h in hubs:
        labels += [h.code + " flights", h.code + " pnrs", h.code + " events"]
    for k in range(12):
        print(file_line(labels[k], texts[k]))
    fl = [parse_flights(names[i * 3], hubs[i], texts[i * 3]) for i in range(4)]
    pn = [parse_pnrs(names[i * 3 + 1], hubs[i], fl[i], texts[i * 3 + 1]) for i in range(4)]
    ev = [parse_events(names[i * 3 + 2], fl[i], texts[i * 3 + 2]) for i in range(4)]
    try:
        parse_pnrs("ORD_pnrs.bad", hubs[0], fl[0], read_text("ORD_pnrs.bad"))
        print("ORD_pnrs.bad: parsed")
    except BatchError as e:
        print(e)
    try:
        parse_events("ATL_events.bad", fl[2], read_text("ATL_events.bad"))
        print("ATL_events.bad: parsed")
    except BatchError as e:
        print(e)
    try:
        parse_flights("JFK_flights.csv", hubs[1], texts[9])
        print("JFK_flights.csv: parsed")
    except BatchError as e:
        print(e)
    same = all([a.key() for a in fl[i]] == [b.key() for b in sched[i]] and
               [a.key() for a in pn[i]] == [b.key() for b in books[i]] and
               [a.key() for a in ev[i]] == [b.key() for b in evs[i]] for i in range(4))
    print("parsed files match the built ones: %s" % ("true" if same else "false"))
    days = [rebook(hubs[i], fl[i], pn[i], ev[i]) for i in range(4)]
    for i in range(4):
        write_text(hubs[i].code + "_rebook.csv", encode_rebook(fl[i], pn[i], days[i]))
    for i in range(4):
        t = read_text(hubs[i].code + "_rebook.csv")
        print(file_line(hubs[i].code + " rebookings", t))
    for i in range(4):
        print(ops_report(hubs[i], fl[i], days[i]))
    for i in range(4):
        for line in rebook_report(hubs[i], pn[i], days[i]):
            print(line)
    for i in range(4):
        print(load_report(hubs[i], fl[i], days[i]))
    for i in range(4):
        print(care_report(hubs[i], fl[i], pn[i], days[i]))
    pool = Pool()
    seq = [0, 1, 2, 3]
    if os.environ.get("FLIGHT_REVERSE") == "1":
        seq = [3, 2, 1, 0]
    for i in seq:
        for line in ask_spares(pool, hubs[i], fl[i], pn[i], days[i]):
            print(line)
    write_text("spares.log", "\n".join(pool.log) + "\n")
    back = read_text("spares.log")
    rows = len(back.split("\n")) - 1
    print("spares.log: %s entries, %s bytes, fnv %08x" % (group3(rows), group3(len(back)), text_sum(back)))
    for p in names + ["ORD_pnrs.bad", "ATL_events.bad", "spares.log"] + [h.code + "_rebook.csv" for h in hubs]:
        os.remove(p)


def main():
    try:
        run()
    except BatchError as e:
        print("flight_rebook failed: %s" % e)


main()
