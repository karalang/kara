"""The Python mirror of milk_collect.kara: a week of milk collection at a
dairy co-operative's four plants. It writes expected.out.

Run: python3 -I -B milk_collect.py > expected.out
     MILK_REVERSE=1 python3 -I -B milk_collect.py   # the plants send surplus to the drier in reverse"""
import os

MASK32 = 4294967295
DAYS = 7
# milk weighs 1030 g a litre; fat and protein are in hundredths of a percent
DENSITY = 1030
# cents per kg of fat and of protein, and per litre hauled
FAT_PRICE = 520
PROTEIN_PRICE = 890
HAULAGE = 4
ORGANIC = 12
# a tank warmer than this (tenths of a degree) is refused at the farm
WARM = 60
# somatic cells (thousands per mL): the bands, and cents per litre in each
SCC_BANDS = [200, 400, 500]
SCC_ADJUST = [2, 0, -3, -8]
SCC_NAMES = ["under 200k", "200k-399k", "400k-499k", "500k and over"]
# bacteria (thousand cfu per mL) over this cost 2 cents a litre
BACTERIA = 100
BACTERIA_CHARGE = 2
# a dumped load is charged to the farms whose samples were positive, at
# this many cents a litre, and they are not collected for two days
DUMP_PRICE = 45
SUSPEND = 3
# cents a litre on what a farm sends over its quota for the week
OVER_QUOTA = 6
REFUSALS = ["warm", "suspended"]
# the co-op's drier takes the plants' surplus for the week, in the order
# they send it, until it is full
DRIER = 150000


class Site:
    def __init__(self, code, digit, seed, farms, routes, tankers, capacity, silo):
        self.code = code
        self.digit = digit
        self.seed = seed
        self.farms = farms
        self.routes = routes
        self.tankers = tankers
        self.capacity = capacity
        self.silo = silo


class Farm:
    def __init__(self, fid, route, herd, tank, every, organic, quota):
        self.fid = fid
        self.route = route
        self.herd = herd
        self.tank = tank
        self.every = every
        self.organic = organic
        self.quota = quota

    def key(self):
        return (self.fid, self.route, self.herd, self.tank, self.every, self.organic, self.quota)


class Tanker:
    def __init__(self, tid, route, capacity):
        self.tid = tid
        self.route = route
        self.capacity = capacity

    def key(self):
        return (self.tid, self.route, self.capacity)


class Pickup:
    def __init__(self, pno, day, minute, fid, litres, temp):
        self.pno = pno
        self.day = day
        self.minute = minute
        self.fid = fid
        self.litres = litres
        self.temp = temp

    def key(self):
        return (self.pno, self.day, self.minute, self.fid, self.litres, self.temp)


class Sample:
    def __init__(self, pno, fat, protein, scc, bacteria, inhibitor):
        self.pno = pno
        self.fat = fat
        self.protein = protein
        self.scc = scc
        self.bacteria = bacteria
        self.inhibitor = inhibitor

    def key(self):
        return (self.pno, self.fat, self.protein, self.scc, self.bacteria, self.inhibitor)


class Load:
    def __init__(self, lno, day, route, tanker, capacity):
        self.lno = lno
        self.day = day
        self.route = route
        self.tanker = tanker
        self.capacity = capacity
        self.picks = []
        self.litres = 0
        self.dumped = False


class Account:
    def __init__(self):
        self.pickups = 0
        self.litres = 0
        self.fat = 0
        self.protein = 0
        self.scc = 0
        self.gross = 0
        self.quality = 0
        self.haulage = 0
        self.charges = 0
        self.over = 0
        self.net = 0


class Week:
    pass


class Drier:
    def __init__(self):
        self.left = DRIER
        self.log = []


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
    a = -c if c < 0 else c
    return "%s$%s.%02d" % (sign, group3(a // 100), a % 100)


def percent(bp):
    return "%d.%02d%%" % (bp // 100, bp % 100)


def clock(m):
    return "%02d:%02d" % (m // 60, m % 60)


def is_digits(s):
    return len(s) > 0 and all("0" <= ch <= "9" for ch in s)


def small_int(t, lo, hi):
    if not is_digits(t) or len(t) > 8:
        return None
    v = int(t)
    if v < lo or v > hi:
        return None
    return v


def time_of(t):
    if len(t) != 5 or t[2] != ":" or not is_digits(t[:2]) or not is_digits(t[3:]):
        return None
    h = int(t[:2])
    m = int(t[3:])
    if h > 23 or m > 59:
        return None
    return h * 60 + m


def file_line(name, text):
    rows = len(text.split("\n")) - 2
    return "%s: %s rows, %s bytes, fnv %08x" % (name, group3(rows), group3(len(text)), text_sum(text))


def tanker_id(site, n):
    return "T%d-%02d" % (site.digit, n)


def band(scc):
    b = 0
    while b < 3 and scc >= SCC_BANDS[b]:
        b += 1
    return b


# ------------------------------------------------------------ building

def build_farms(site):
    out = []
    for k in range(site.farms):
        r = mix(site.seed, k, 11)
        herd = 30 + (r >> 8) % 200
        litres = herd * (22 + (r >> 16) % 9)
        every = 1 if herd >= 120 else 2
        tank = (litres * every * 106 // 100 + 499) // 500 * 500
        out.append(Farm(site.digit * 1000 + 1 + k, 1 + r % site.routes, herd, tank, every,
                        (r >> 24) % 100 < 12, herd * 25))
    return out


def build_tankers(site):
    sizes = [18000, 24000, 30000]
    out = []
    n = 0
    for route in range(1, site.routes + 1):
        for j in range(site.tankers):
            n += 1
            out.append(Tanker(tanker_id(site, n), route, sizes[mix(site.seed, n, 21) % 3]))
    return out


def build_pickups(site, farms):
    pos = {}
    count = [0] * (site.routes + 1)
    for f in farms:
        pos[f.fid] = count[f.route]
        count[f.route] += 1
    out = []
    n = 0
    for d in range(1, DAYS + 1):
        day = []
        for f in farms:
            if (d + f.fid) % f.every != 0:
                continue
            r = mix(site.seed, d * 10000 + f.fid, 31)
            y = 22 + (mix(site.seed, f.fid - site.digit * 1000 - 1, 11) >> 16) % 9
            litres = f.herd * y * f.every * (95 + r % 11) // 100
            minute = 300 + (f.route - 1) * 11 + pos[f.fid] * 17 + (r >> 8) % 9
            temp = 30 + (r >> 12) % 16
            if (r >> 20) % 100 < 2:
                temp = 61 + (r >> 4) % 30
            day.append((minute, f.fid, litres, temp))
        day.sort()
        for (minute, fid, litres, temp) in day:
            n += 1
            out.append(Pickup(n, d, minute, fid, litres, temp))
    return out


def build_samples(site, pickups):
    out = []
    for p in pickups:
        q = mix(site.seed, p.pno, 41)
        b = mix(site.seed, p.fid, 14)
        fat = 380 + b % 80 + q % 41 - 20
        protein = 310 + (b >> 8) % 50 + (q >> 8) % 21 - 10
        scc = (80 + (b >> 16) % 450) * (90 + (q >> 12) % 21) // 100
        bacteria = 5 + (q >> 18) % 60
        if (q >> 24) % 100 < 4:
            bacteria = 120 + (q >> 4) % 300
        out.append(Sample(p.pno, fat, protein, scc, bacteria, mix(site.seed, p.pno, 42) % 1000 < 6))
    return out


# ------------------------------------------------------------ files

FARM_HEADER = "farm,route,herd,tank,every,organic,quota"
TANKER_HEADER = "tanker,route,capacity"
PICKUP_HEADER = "pickup,day,time,farm,litres,temperature"
SAMPLE_HEADER = "pickup,fat,protein,scc,bacteria,inhibitor"
LOAD_HEADER = "load,day,route,tanker,pickups,litres,result"
PAYMENT_HEADER = "farm,pickups,litres,fat,protein,scc,gross,quality,haulage,charges,over_quota,net"


def encode_farms(farms):
    lines = [FARM_HEADER]
    for f in farms:
        lines.append("%d,%d,%d,%d,%d,%s,%d" % (f.fid, f.route, f.herd, f.tank, f.every, "Y" if f.organic else "N",
                                             f.quota))
    return "\n".join(lines) + "\n"


def encode_tankers(tankers):
    lines = [TANKER_HEADER]
    for t in tankers:
        lines.append("%s,%d,%d" % (t.tid, t.route, t.capacity))
    return "\n".join(lines) + "\n"


def encode_pickups(pickups):
    lines = [PICKUP_HEADER]
    for p in pickups:
        lines.append("%d,%d,%s,%d,%d,%d" % (p.pno, p.day, clock(p.minute), p.fid, p.litres, p.temp))
    return "\n".join(lines) + "\n"


def encode_samples(samples):
    lines = [SAMPLE_HEADER]
    for s in samples:
        lines.append("%d,%d,%d,%d,%d,%s" % (s.pno, s.fat, s.protein, s.scc, s.bacteria, "P" if s.inhibitor else "N"))
    return "\n".join(lines) + "\n"


def encode_loads(w):
    lines = [LOAD_HEADER]
    for ld in w.loads:
        lines.append("%d,%d,%d,%s,%d,%d,%s" % (ld.lno, ld.day, ld.route, ld.tanker, len(ld.picks), ld.litres,
                                              "DUMPED" if ld.dumped else "OK"))
    return "\n".join(lines) + "\n"


def encode_payments(w):
    lines = [PAYMENT_HEADER]
    for (fid, a) in w.accounts:
        lines.append("%d,%d,%d,%d,%d,%d,%d,%d,%d,%d,%d,%d" % (fid, a.pickups, a.litres, a.fat, a.protein, a.scc, a.gross,
                                                             a.quality, a.haulage, a.charges, a.over, a.net))
    return "\n".join(lines) + "\n"


def read_text(path):
    try:
        with open(path) as f:
            return f.read()
    except OSError:
        raise BatchError("cannot read " + path)


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


def parse_farms(path, site, text):
    out = []
    seen = set()
    n = 1
    for line in body_lines(path, text, FARM_HEADER):
        n += 1
        f = fields_of(path, n, line, 7)
        fid = small_int(f[0], 0, 9999) if len(f[0]) == 4 else None
        if fid is None or fid // 1000 != site.digit:
            raise BatchError("%s line %d: bad farm id '%s'" % (path, n, f[0]))
        if fid in seen:
            raise BatchError("%s line %d: farm %d twice" % (path, n, fid))
        seen.add(fid)
        route = small_int(f[1], 1, site.routes)
        if route is None:
            raise BatchError("%s line %d: no route '%s' at %s" % (path, n, f[1], site.code))
        herd = small_int(f[2], 1, 5000)
        if herd is None:
            raise BatchError("%s line %d: bad herd '%s'" % (path, n, f[2]))
        tank = small_int(f[3], 100, 999999)
        if tank is None:
            raise BatchError("%s line %d: bad tank '%s'" % (path, n, f[3]))
        every = small_int(f[4], 1, 2)
        if every is None:
            raise BatchError("%s line %d: bad pickup interval '%s'" % (path, n, f[4]))
        if f[5] != "Y" and f[5] != "N":
            raise BatchError("%s line %d: bad organic flag '%s'" % (path, n, f[5]))
        quota = small_int(f[6], 0, 999999)
        if quota is None:
            raise BatchError("%s line %d: bad quota '%s'" % (path, n, f[6]))
        out.append(Farm(fid, route, herd, tank, every, f[5] == "Y", quota))
    return out


def parse_tankers(path, site, text):
    out = []
    seen = set()
    n = 1
    for line in body_lines(path, text, TANKER_HEADER):
        n += 1
        f = fields_of(path, n, line, 3)
        t = f[0]
        if len(t) != 5 or t[0] != "T" or t[2] != "-" or not is_digits(t[1]) or not is_digits(t[3:]):
            raise BatchError("%s line %d: bad tanker '%s'" % (path, n, t))
        if int(t[1]) != site.digit:
            raise BatchError("%s line %d: tanker %s is not at %s" % (path, n, t, site.code))
        if t in seen:
            raise BatchError("%s line %d: tanker %s twice" % (path, n, t))
        seen.add(t)
        route = small_int(f[1], 1, site.routes)
        if route is None:
            raise BatchError("%s line %d: no route '%s' at %s" % (path, n, f[1], site.code))
        cap = small_int(f[2], 1000, 60000)
        if cap is None:
            raise BatchError("%s line %d: bad capacity '%s'" % (path, n, f[2]))
        out.append(Tanker(t, route, cap))
    for r in range(1, site.routes + 1):
        if not any(t.route == r for t in out):
            raise BatchError("%s: no tanker on route %d" % (path, r))
    return out


def parse_pickups(path, site, farms, text):
    tanks = {}
    for f in farms:
        tanks[f.fid] = f.tank
    out = []
    n = 1
    last = 0
    for line in body_lines(path, text, PICKUP_HEADER):
        n += 1
        f = fields_of(path, n, line, 6)
        if f[0] != str(n - 1):
            raise BatchError("%s line %d: pickup '%s' out of order" % (path, n, f[0]))
        day = small_int(f[1], 1, DAYS)
        if day is None:
            raise BatchError("%s line %d: bad day '%s'" % (path, n, f[1]))
        minute = time_of(f[2])
        if minute is None:
            raise BatchError("%s line %d: bad time '%s'" % (path, n, f[2]))
        if day * 1440 + minute < last:
            raise BatchError("%s line %d: time goes backwards" % (path, n))
        last = day * 1440 + minute
        fid = small_int(f[3], 0, 9999)
        if fid is None or fid not in tanks:
            raise BatchError("%s line %d: no farm %s" % (path, n, f[3]))
        litres = small_int(f[4], 1, 999999)
        if litres is None:
            raise BatchError("%s line %d: bad litres '%s'" % (path, n, f[4]))
        if litres > tanks[fid]:
            raise BatchError("%s line %d: %d litres is more than farm %d's tank" % (path, n, litres, fid))
        temp = small_int(f[5], 0, 400)
        if temp is None:
            raise BatchError("%s line %d: bad temperature '%s'" % (path, n, f[5]))
        out.append(Pickup(n - 1, day, minute, fid, litres, temp))
    return out


def parse_samples(path, site, pickups, text):
    out = []
    n = 1
    for line in body_lines(path, text, SAMPLE_HEADER):
        n += 1
        f = fields_of(path, n, line, 6)
        if n - 1 > len(pickups) or f[0] != str(n - 1):
            raise BatchError("%s line %d: no pickup '%s'" % (path, n, f[0]))
        fat = small_int(f[1], 100, 900)
        if fat is None:
            raise BatchError("%s line %d: bad fat '%s'" % (path, n, f[1]))
        protein = small_int(f[2], 100, 900)
        if protein is None:
            raise BatchError("%s line %d: bad protein '%s'" % (path, n, f[2]))
        scc = small_int(f[3], 1, 9999)
        if scc is None:
            raise BatchError("%s line %d: bad cell count '%s'" % (path, n, f[3]))
        bacteria = small_int(f[4], 0, 9999)
        if bacteria is None:
            raise BatchError("%s line %d: bad bacteria count '%s'" % (path, n, f[4]))
        if f[5] != "P" and f[5] != "N":
            raise BatchError("%s line %d: bad inhibitor result '%s'" % (path, n, f[5]))
        out.append(Sample(n - 1, fat, protein, scc, bacteria, f[5] == "P"))
    if len(out) != len(pickups):
        raise BatchError("%s: %d samples for %d pickups" % (path, len(out), len(pickups)))
    return out


# ------------------------------------------------------------ the week

def deliver(w, ld, day, pickups, samples, fidx):
    positive = []
    for i in ld.picks:
        if samples[i].inhibitor and pickups[i].fid not in positive:
            positive.append(pickups[i].fid)
    if positive:
        ld.dumped = True
        w.dumped += 1
        w.dumped_litres += ld.litres
        charge = ld.litres * DUMP_PRICE // len(positive)
        for fid in positive:
            k = fidx[fid]
            w.charges[k] += charge
            w.charged_farms.add(fid)
            w.until[k] = day + SUSPEND
        for i in ld.picks:
            if pickups[i].fid not in positive:
                w.paid.append(i)
        return
    w.received[day - 1] += ld.litres
    for i in ld.picks:
        w.paid.append(i)


def close_week(site, farms, tankers, pickups, samples):
    w = Week()
    fidx = {}
    for k in range(len(farms)):
        fidx[farms[k].fid] = k
    by_route = [[] for _ in range(site.routes + 1)]
    for t in tankers:
        by_route[t.route].append(t)
    w.loads = []
    w.paid = []
    w.until = [0] * len(farms)
    w.charges = [0] * len(farms)
    w.charged_farms = set()
    w.refused = [0, 0]
    w.refused_litres = 0
    w.collected = 0
    w.litres = 0
    w.by_day = [0] * DAYS
    w.received = [0] * DAYS
    w.processed = [0] * DAYS
    w.dumped = 0
    w.dumped_litres = 0
    w.silo = 0
    w.surplus = 0
    w.full_days = 0
    trips = [0] * (site.routes + 1)
    route_loads = [0] * (site.routes + 1)
    lno = 0
    i = 0
    for day in range(1, DAYS + 1):
        open_load = [None] * (site.routes + 1)
        while i < len(pickups) and pickups[i].day == day:
            p = pickups[i]
            k = fidx[p.fid]
            if day < w.until[k]:
                w.refused[1] += 1
                w.refused_litres += p.litres
                i += 1
                continue
            if p.temp > WARM:
                w.refused[0] += 1
                w.refused_litres += p.litres
                i += 1
                continue
            r = farms[k].route
            ld = open_load[r]
            if ld is not None and ld.litres + p.litres > ld.capacity:
                deliver(w, ld, day, pickups, samples, fidx)
                ld = None
            if ld is None:
                t = by_route[r][trips[r] % len(by_route[r])]
                trips[r] += 1
                route_loads[r] += 1
                lno += 1
                ld = Load(lno, day, r, t.tid, t.capacity)
                w.loads.append(ld)
                open_load[r] = ld
            ld.picks.append(i)
            ld.litres += p.litres
            w.collected += 1
            w.litres += p.litres
            w.by_day[day - 1] += 1
            i += 1
        for r in range(1, site.routes + 1):
            if open_load[r] is not None:
                deliver(w, open_load[r], day, pickups, samples, fidx)
        w.silo += w.received[day - 1]
        done = min(w.silo, site.capacity)
        w.processed[day - 1] = done
        w.silo -= done
        if done == site.capacity:
            w.full_days += 1
        if w.silo > site.silo:
            w.surplus += w.silo - site.silo
            w.silo = site.silo
    w.route_loads = route_loads
    acc = [Account() for _ in farms]
    wf = [0] * len(farms)
    wp = [0] * len(farms)
    ws = [0] * len(farms)
    w.bacteria = 0
    for idx in sorted(w.paid):
        p = pickups[idx]
        s = samples[idx]
        k = fidx[p.fid]
        a = acc[k]
        a.pickups += 1
        a.litres += p.litres
        wf[k] += s.fat * p.litres
        wp[k] += s.protein * p.litres
        ws[k] += s.scc * p.litres
        fat_g = p.litres * DENSITY * s.fat // 10000
        prot_g = p.litres * DENSITY * s.protein // 10000
        a.gross += fat_g * FAT_PRICE // 1000 + prot_g * PROTEIN_PRICE // 1000
        q = SCC_ADJUST[band(s.scc)] * p.litres
        if s.bacteria > BACTERIA:
            q -= BACTERIA_CHARGE * p.litres
            w.bacteria += 1
        if farms[k].organic:
            q += ORGANIC * p.litres
        a.quality += q
        a.haulage += HAULAGE * p.litres
    w.accounts = []
    for k in range(len(farms)):
        a = acc[k]
        if a.litres > 0:
            a.fat = wf[k] // a.litres
            a.protein = wp[k] // a.litres
            a.scc = ws[k] // a.litres
        a.charges = w.charges[k]
        allowed = farms[k].quota * DAYS
        if a.litres > allowed:
            a.over = (a.litres - allowed) * OVER_QUOTA
        a.net = a.gross + a.quality - a.haulage - a.charges - a.over
        w.accounts.append((farms[k].fid, a))
    return w


# ------------------------------------------------------------ reports

def collection_report(site, w):
    return "%s collections: %s pickups, %s litres, %d refused (%d warm, %d suspended, %s litres); by day %s" % (
        site.code, group3(w.collected), group3(w.litres), sum(w.refused), w.refused[0], w.refused[1],
        group3(w.refused_litres), " ".join(str(x) for x in w.by_day))


def quality_report(site, w):
    litres = 0
    f = 0
    p = 0
    s = 0
    bands = [0, 0, 0, 0]
    for (_, a) in w.accounts:
        if a.litres == 0:
            continue
        litres += a.litres
        f += a.fat * a.litres
        p += a.protein * a.litres
        s += a.scc * a.litres
        bands[band(a.scc)] += 1
    parts = ", ".join("%d %s" % (bands[b], SCC_NAMES[b]) for b in range(4))
    return "%s quality: fat %s, protein %s, cells %dk; farms by cell count %s; %d pickups over %dk bacteria" % (
        site.code, percent(f // litres), percent(p // litres), s // litres, parts, w.bacteria, BACTERIA)


def load_report(site, w):
    busiest = 1
    for r in range(1, site.routes + 1):
        if w.route_loads[r] > w.route_loads[busiest]:
            busiest = r
    full = 0
    for ld in w.loads:
        if ld.litres * 10 >= ld.capacity * 9:
            full += 1
    charged = sum(w.charges)
    return "%s loads: %d tanker loads on %d routes, %d at least 90%% full; %d dumped (%s litres) for inhibitors from %d farms, charged %s; busiest route %d with %d loads" % (
        site.code, len(w.loads), site.routes, full, w.dumped, group3(w.dumped_litres), len(w.charged_farms),
        money(charged), busiest, w.route_loads[busiest])


def intake_report(site, w):
    return "%s intake: received %s litres, processed %s, %s in the silo at the end, %s surplus; %d days at capacity; by day %s" % (
        site.code, group3(sum(w.received)), group3(sum(w.processed)), group3(w.silo), group3(w.surplus),
        w.full_days, " ".join(group3(x) for x in w.processed))


def payment_report(site, w):
    paid = 0
    litres = 0
    bonus = 0
    penalty = 0
    over = 0
    negative = 0
    top = None
    for (fid, a) in w.accounts:
        paid += a.net
        litres += a.litres
        if a.quality > 0:
            bonus += a.quality
        else:
            penalty -= a.quality
        over += a.over
        if a.net < 0:
            negative += 1
        if top is None or a.net > top[1]:
            top = (fid, a.net)
    return "%s payments: %d farms, %s paid, %d.%02d c/L; quality bonus %s, penalties %s, over quota %s; %d farms owe; top farm %d %s" % (
        site.code, len(w.accounts), money(paid), paid // litres, paid * 100 // litres % 100, money(bonus),
        money(penalty), money(over), negative, top[0], money(top[1]))


# ------------------------------------------------------------ the drier

def send(drier, site, w):
    asked = w.surplus
    taken = min(asked, drier.left)
    drier.left -= taken
    drier.log.append("%s,%d,%d,%d" % (site.code, asked, taken, drier.left))
    return "%s drier: sent %s litres of surplus, %s dried, %s sold on the spot market; %s litres of room left" % (
        site.code, group3(asked), group3(taken), group3(asked - taken), group3(drier.left))


# ------------------------------------------------------------ the job

def run():
    val = Site("VAL", 1, 9101, 120, 4, 2, 336000, 60000)
    hil = Site("HIL", 2, 9202, 90, 3, 2, 247000, 40000)
    coa = Site("COA", 3, 9303, 140, 5, 3, 425000, 70000)
    lak = Site("LAK", 4, 9404, 70, 3, 1, 231000, 30000)
    sites = [val, hil, coa, lak]

    fm = [build_farms(s) for s in sites]
    tk = [build_tankers(s) for s in sites]
    pk = [build_pickups(sites[i], fm[i]) for i in range(4)]
    sm = [build_samples(sites[i], pk[i]) for i in range(4)]

    kinds = [("farms", encode_farms, fm), ("tankers", encode_tankers, tk), ("pickups", encode_pickups, pk),
             ("samples", encode_samples, sm)]
    for i in range(4):
        for (name, enc, data) in kinds:
            with open("%s_%s.csv" % (sites[i].code, name), "w") as f:
                f.write(enc(data[i]))

    whole = read_text("VAL_pickups.csv")
    with open("VAL_pickups.bad", "w") as f:
        f.write(damage(whole, 40, 3, "1999"))
    whole2 = read_text("HIL_samples.csv")
    with open("HIL_samples.bad", "w") as f:
        f.write(damage(whole2, 30, 5, "X"))

    texts = []
    for i in range(4):
        row = {}
        for (name, _, _) in kinds:
            row[name] = read_text("%s_%s.csv" % (sites[i].code, name))
        texts.append(row)
    for i in range(4):
        for (name, _, _) in kinds:
            print(file_line("%s %s" % (sites[i].code, name), texts[i][name]))

    pf = [parse_farms("%s_farms.csv" % sites[i].code, sites[i], texts[i]["farms"]) for i in range(4)]
    pt = [parse_tankers("%s_tankers.csv" % sites[i].code, sites[i], texts[i]["tankers"]) for i in range(4)]
    pp = [parse_pickups("%s_pickups.csv" % sites[i].code, sites[i], pf[i], texts[i]["pickups"]) for i in range(4)]
    ps = [parse_samples("%s_samples.csv" % sites[i].code, sites[i], pp[i], texts[i]["samples"]) for i in range(4)]
    for (path, fn) in [("VAL_pickups.bad", lambda x: parse_pickups("VAL_pickups.bad", val, pf[0], x)),
                       ("HIL_samples.bad", lambda x: parse_samples("HIL_samples.bad", hil, pp[1], x))]:
        try:
            fn(read_text(path))
            print(path + ": parsed")
        except BatchError as e:
            print(str(e))
    try:
        parse_tankers("COA_tankers.csv", lak, texts[2]["tankers"])
        print("COA_tankers.csv: parsed")
    except BatchError as e:
        print(str(e))
    same = True
    for i in range(4):
        for (a, b) in [(pf[i], fm[i]), (pt[i], tk[i]), (pp[i], pk[i]), (ps[i], sm[i])]:
            if [x.key() for x in a] != [x.key() for x in b]:
                same = False
    print("parsed files match the built ones: %s" % ("true" if same else "false"))

    w = [close_week(sites[i], pf[i], pt[i], pp[i], ps[i]) for i in range(4)]

    for i in range(4):
        with open("%s_loads.csv" % sites[i].code, "w") as f:
            f.write(encode_loads(w[i]))
        with open("%s_payments.csv" % sites[i].code, "w") as f:
            f.write(encode_payments(w[i]))
    for i in range(4):
        print(file_line("%s loads" % sites[i].code, read_text("%s_loads.csv" % sites[i].code)))
        print(file_line("%s payments" % sites[i].code, read_text("%s_payments.csv" % sites[i].code)))

    for fn in [collection_report, quality_report, load_report, intake_report, payment_report]:
        for i in range(4):
            print(fn(sites[i], w[i]))

    drier = Drier()
    order = [0, 1, 2, 3]
    if os.environ.get("MILK_REVERSE") == "1":
        order = [3, 2, 1, 0]
    for i in order:
        print(send(drier, sites[i], w[i]))
    with open("drier.log", "w") as f:
        f.write("\n".join(drier.log) + "\n")
    back = read_text("drier.log")
    print("drier.log: %s entries, %s bytes, fnv %08x" % (group3(len(back.split("\n")) - 1), group3(len(back)),
                                                        text_sum(back)))

    for i in range(4):
        for name in ["farms", "tankers", "pickups", "samples", "loads", "payments"]:
            os.remove("%s_%s.csv" % (sites[i].code, name))
    for p in ["VAL_pickups.bad", "HIL_samples.bad", "drier.log"]:
        os.remove(p)


def main():
    try:
        run()
    except BatchError as e:
        print("milk_collect failed: %s" % e)


main()
