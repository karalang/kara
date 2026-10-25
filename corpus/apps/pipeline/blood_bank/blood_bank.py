"""The Python mirror of blood_bank.kara: a week at a regional blood service's
four donation centres. It writes expected.out.

Run: python3 -I -B blood_bank.py > expected.out
     BLOOD_REVERSE=1 python3 -I -B blood_bank.py   # the centres draw on the reserve in reverse"""
import os

MASK32 = 4294967295
DAYS = 7
YEAR = 2026
GROUPS = ["O-", "O+", "A-", "A+", "B-", "B+", "AB-", "AB+"]
# how common each group is, per thousand donors and per thousand patients
SHARE = [70, 380, 60, 340, 20, 90, 10, 30]
# the ABO antigens on the red cells of each group: A is 1, B is 2
ANTIGENS = [0, 0, 1, 1, 2, 2, 3, 3]
COMPONENTS = ["RBC", "PLT", "FFP"]
NAMES = ["red cells", "platelets", "plasma"]
# days a component keeps from the day it is collected
SHELF = [42, 5, 365]
# whole blood is not taken within this many days of the last whole blood
# donation, or within APH_GAP of the last apheresis; apheresis not within
# APH_GAP of either, or WB_AFTER_APH of whole blood
WB_GAP = 56
APH_GAP = 7
APH_AFTER_WB = 28
MIN_HB = 125
MIN_WEIGHT = 50
# donors of at least this weight give a double apheresis collection
SPLIT_WEIGHT = 70
POOL = 4
FOREVER = 10000
REASONS = ["earlier deferral", "age", "weight", "low hemoglobin", "too soon"]
MARKERS = ["HBV", "HCV", "HIV", "syphilis"]
# reactive results per ten thousand tests, by marker
REACTIVE = [40, 30, 20, 60]
# the national reserve of O-negative red cells for the week: it covers the
# centres' red cell shortfalls in the order they ask, from what is left
RESERVE = 45


class Site:
    def __init__(self, code, digit, seed, donors, visits, hospitals, stock, orders):
        self.code = code
        self.digit = digit
        self.seed = seed
        self.donors = donors
        self.visits = visits
        self.hospitals = hospitals
        self.stock = stock
        self.orders = orders


class Donor:
    def __init__(self, did, group, born, weight, last_wb, last_aph, deferred):
        self.did = did
        self.group = group
        self.born = born
        self.weight = weight
        self.last_wb = last_wb
        self.last_aph = last_aph
        self.deferred = deferred

    def key(self):
        return (self.did, self.group, self.born, self.weight, self.last_wb, self.last_aph, self.deferred)


class Unit:
    def __init__(self, uid, comp, group, collected):
        self.uid = uid
        self.comp = comp
        self.group = group
        self.collected = collected

    def key(self):
        return (self.uid, self.comp, self.group, self.collected)


class Visit:
    def __init__(self, vno, day, minute, did, kind, hb):
        self.vno = vno
        self.day = day
        self.minute = minute
        self.did = did
        self.kind = kind
        self.hb = hb

    def key(self):
        return (self.vno, self.day, self.minute, self.did, self.kind, self.hb)


class Test:
    def __init__(self, vno, results, typed):
        self.vno = vno
        self.results = results
        self.typed = typed

    def key(self):
        return (self.vno, tuple(self.results), self.typed)


class Order:
    def __init__(self, ono, day, hospital, comp, group, units, urgent):
        self.ono = ono
        self.day = day
        self.hospital = hospital
        self.comp = comp
        self.group = group
        self.units = units
        self.urgent = urgent

    def key(self):
        return (self.ono, self.day, self.hospital, self.comp, self.group, self.units, self.urgent)


class Stock:
    def __init__(self, uid, comp, group, collected, exp):
        self.uid = uid
        self.comp = comp
        self.group = group
        self.collected = collected
        self.exp = exp
        self.gone = False


class Week:
    pass


class Reserve:
    def __init__(self):
        self.left = RESERVE
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


def plural(n, word):
    return "%s %s%s" % (group3(n), word, "" if n == 1 else "s")


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


# "-" or a day before the week: "-N" for N days before it, or "0"
def past_day(t):
    if t == "-":
        return -FOREVER
    if t == "0":
        return 0
    if not t.startswith("-"):
        return None
    v = small_int(t[1:], 1, 9999)
    if v is None:
        return None
    return -v


def show_past(d):
    return "-" if d == -FOREVER else str(d)


def file_line(name, text):
    rows = len(text.split("\n")) - 2
    return "%s: %s rows, %s bytes, fnv %08x" % (name, group3(rows), group3(len(text)), text_sum(text))


def group_of(name):
    for g in range(8):
        if GROUPS[g] == name:
            return g
    return -1


def comp_of(name):
    for c in range(3):
        if COMPONENTS[c] == name:
            return c
    return -1


def pick_group(r):
    p = r % 1000
    for g in range(8):
        if p < SHARE[g]:
            return g
        p -= SHARE[g]
    return 7


def rh(g):
    return g % 2


# Whether a unit of group d may go to a patient of group r.
def compatible(comp, d, r, any_rh):
    if comp == 0:
        return (ANTIGENS[d] & ~ANTIGENS[r]) == 0 and (any_rh or rh(d) <= rh(r))
    if comp == 2:
        return (ANTIGENS[r] & ~ANTIGENS[d]) == 0
    return True


# How many patient groups a unit of group d can serve: the lower, the
# sooner it is used, so the universal units are kept for those who need them.
def reach(comp, d):
    n = 0
    for r in range(8):
        if compatible(comp, d, r, False):
            n += 1
    return n


def hospital_code(site, h):
    return "%s-H%d" % (site.code, h + 1)


# ------------------------------------------------------------ building

def build_donors(site):
    out = []
    for k in range(site.donors):
        r = mix(site.seed, k, 11)
        q = mix(site.seed, k, 12)
        born = 1956 + r % 53
        if (r >> 8) % 80 == 0:
            born = 2009
        elif (r >> 8) % 80 == 1:
            born = 1952
        weight = 48 + (r >> 12) % 70
        last_wb = -FOREVER
        if q % 100 < 60:
            last_wb = -(1 + (q >> 8) % 700)
        last_aph = -FOREVER
        if (q >> 16) % 100 < 15:
            last_aph = -(1 + (q >> 20) % 60)
        deferred = 0
        if (r >> 20) % 100 < 3:
            deferred = 1 + (q >> 4) % 20
        out.append(Donor(site.digit * 100000 + 1 + k, pick_group(mix(site.seed, k, 13)), born, weight,
                         last_wb, last_aph, deferred))
    return out


def build_stock(site):
    out = []
    for k in range(site.stock):
        r = mix(site.seed, k, 21)
        p = r % 100
        comp = 0 if p < 60 else (1 if p < 75 else 2)
        age = (r >> 8) % [41, 5, 300][comp]
        out.append(Unit("U%d%06d" % (site.digit, k + 1), comp, pick_group(mix(site.seed, k, 22)), -age))
    return out


def build_visits(site, donors):
    out = []
    n = 0
    for d in range(1, DAYS + 1):
        count = site.visits + mix(site.seed, d, 31) % 11 - 5
        day = []
        for j in range(count):
            r = mix(site.seed, d * 1000 + j, 32)
            k = r % len(donors)
            minute = 480 + (r >> 8) % 720
            kind = "APH" if (r >> 20) % 100 < 18 else "WB"
            hb = 120 + mix(site.seed, d * 1000 + j, 33) % 55
            day.append((minute, j, donors[k].did, kind, hb))
        day.sort()
        for (minute, j, did, kind, hb) in day:
            n += 1
            out.append(Visit(n, d, minute, did, kind, hb))
    return out


def build_tests(site, visits, donors):
    by_id = {}
    for dn in donors:
        by_id[dn.did] = dn
    out = []
    for v in visits:
        r = mix(site.seed, v.vno, 41)
        results = []
        for m in range(4):
            q = mix(site.seed, v.vno * 4 + m, 42)
            results.append(q % 10000 < REACTIVE[m])
        g = by_id[v.did].group
        if (r >> 8) % 1000 < 5:
            g = (g + 1 + r % 7) % 8
        out.append(Test(v.vno, results, g))
    return out


def build_orders(site):
    out = []
    n = 0
    for d in range(1, DAYS + 1):
        count = site.orders + mix(site.seed, d, 51) % 5
        for j in range(count):
            r = mix(site.seed, d * 1000 + j, 52)
            p = r % 100
            comp = 0 if p < 60 else (1 if p < 80 else 2)
            units = 1 + (r >> 8) % [5, 2, 4][comp]
            n += 1
            out.append(Order(n, d, hospital_code(site, (r >> 12) % site.hospitals), comp,
                             pick_group(mix(site.seed, d * 1000 + j, 53)), units, (r >> 20) % 100 < 15))
    return out


# ------------------------------------------------------------ files

DONOR_HEADER = "donor,group,born,weight,last_whole_blood,last_apheresis,deferred_until"
STOCK_HEADER = "unit,component,group,collected"
VISIT_HEADER = "visit,day,time,donor,kind,hemoglobin"
TEST_HEADER = "visit,hbv,hcv,hiv,syphilis,abo"
ORDER_HEADER = "order,day,hospital,component,group,units,priority"
ISSUE_HEADER = "unit,order,day,hospital,component,unit_group,patient_group,expires"
INVENTORY_HEADER = "unit,component,group,collected,expires"


def encode_donors(donors):
    lines = [DONOR_HEADER]
    for d in donors:
        lines.append("%d,%s,%d,%d,%s,%s,%s" % (d.did, GROUPS[d.group], d.born, d.weight, show_past(d.last_wb),
                                              show_past(d.last_aph), "-" if d.deferred == 0 else str(d.deferred)))
    return "\n".join(lines) + "\n"


def encode_stock(units):
    lines = [STOCK_HEADER]
    for u in units:
        lines.append("%s,%s,%s,%d" % (u.uid, COMPONENTS[u.comp], GROUPS[u.group], u.collected))
    return "\n".join(lines) + "\n"


def encode_visits(visits):
    lines = [VISIT_HEADER]
    for v in visits:
        lines.append("%d,%d,%s,%d,%s,%d" % (v.vno, v.day, clock(v.minute), v.did, v.kind, v.hb))
    return "\n".join(lines) + "\n"


def encode_tests(tests):
    lines = [TEST_HEADER]
    for t in tests:
        lines.append("%d,%s,%s" % (t.vno, ",".join("R" if x else "N" for x in t.results), GROUPS[t.typed]))
    return "\n".join(lines) + "\n"


def encode_orders(orders):
    lines = [ORDER_HEADER]
    for o in orders:
        lines.append("%d,%d,%s,%s,%s,%d,%s" % (o.ono, o.day, o.hospital, COMPONENTS[o.comp], GROUPS[o.group],
                                              o.units, "URGENT" if o.urgent else "ROUTINE"))
    return "\n".join(lines) + "\n"


def encode_issues(w):
    lines = [ISSUE_HEADER]
    for (uid, ono, day, hospital, comp, ug, pg, exp) in w.issues:
        lines.append("%s,%d,%d,%s,%s,%s,%s,%d" % (uid, ono, day, hospital, COMPONENTS[comp], GROUPS[ug], GROUPS[pg],
                                                  exp))
    return "\n".join(lines) + "\n"


def encode_inventory(w):
    lines = [INVENTORY_HEADER]
    for s in w.stock:
        if not s.gone:
            lines.append("%s,%s,%s,%d,%d" % (s.uid, COMPONENTS[s.comp], GROUPS[s.group], s.collected, s.exp))
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


def parse_donors(path, site, text):
    out = []
    seen = set()
    n = 1
    for line in body_lines(path, text, DONOR_HEADER):
        n += 1
        f = fields_of(path, n, line, 7)
        did = small_int(f[0], 0, 999999) if len(f[0]) == 6 else None
        if did is None or did // 100000 != site.digit:
            raise BatchError("%s line %d: bad donor id '%s'" % (path, n, f[0]))
        if did in seen:
            raise BatchError("%s line %d: donor %d twice" % (path, n, did))
        seen.add(did)
        g = group_of(f[1])
        if g < 0:
            raise BatchError("%s line %d: bad group '%s'" % (path, n, f[1]))
        born = small_int(f[2], 1900, YEAR) if len(f[2]) == 4 else None
        if born is None:
            raise BatchError("%s line %d: bad birth year '%s'" % (path, n, f[2]))
        weight = small_int(f[3], 30, 250)
        if weight is None:
            raise BatchError("%s line %d: bad weight '%s'" % (path, n, f[3]))
        last_wb = past_day(f[4])
        if last_wb is None:
            raise BatchError("%s line %d: bad last donation '%s'" % (path, n, f[4]))
        last_aph = past_day(f[5])
        if last_aph is None:
            raise BatchError("%s line %d: bad last apheresis '%s'" % (path, n, f[5]))
        deferred = 0
        if f[6] != "-":
            deferred = small_int(f[6], 1, 9999)
            if deferred is None:
                raise BatchError("%s line %d: bad deferral '%s'" % (path, n, f[6]))
        out.append(Donor(did, g, born, weight, last_wb, last_aph, deferred))
    return out


def parse_stock(path, site, text):
    out = []
    seen = set()
    n = 1
    for line in body_lines(path, text, STOCK_HEADER):
        n += 1
        f = fields_of(path, n, line, 4)
        u = f[0]
        if len(u) != 8 or u[0] != "U" or not is_digits(u[1:]) or int(u[1]) != site.digit:
            raise BatchError("%s line %d: bad unit '%s'" % (path, n, u))
        if u in seen:
            raise BatchError("%s line %d: unit %s twice" % (path, n, u))
        seen.add(u)
        c = comp_of(f[1])
        if c < 0:
            raise BatchError("%s line %d: bad component '%s'" % (path, n, f[1]))
        g = group_of(f[2])
        if g < 0:
            raise BatchError("%s line %d: bad group '%s'" % (path, n, f[2]))
        col = past_day(f[3])
        if col is None or col == -FOREVER:
            raise BatchError("%s line %d: bad collection day '%s'" % (path, n, f[3]))
        if col + SHELF[c] <= 0:
            raise BatchError("%s line %d: unit %s expired before the week" % (path, n, u))
        out.append(Unit(u, c, g, col))
    return out


def parse_visits(path, site, donors, text):
    ids = set(d.did for d in donors)
    out = []
    n = 1
    last = 0
    for line in body_lines(path, text, VISIT_HEADER):
        n += 1
        f = fields_of(path, n, line, 6)
        if f[0] != str(n - 1):
            raise BatchError("%s line %d: visit '%s' out of order" % (path, n, f[0]))
        day = small_int(f[1], 1, DAYS)
        if day is None:
            raise BatchError("%s line %d: bad day '%s'" % (path, n, f[1]))
        minute = time_of(f[2])
        if minute is None:
            raise BatchError("%s line %d: bad time '%s'" % (path, n, f[2]))
        if day * 1440 + minute < last:
            raise BatchError("%s line %d: time goes backwards" % (path, n))
        last = day * 1440 + minute
        did = small_int(f[3], 0, 999999)
        if did is None or did not in ids:
            raise BatchError("%s line %d: no donor %s" % (path, n, f[3]))
        if f[4] != "WB" and f[4] != "APH":
            raise BatchError("%s line %d: bad kind '%s'" % (path, n, f[4]))
        hb = small_int(f[5], 50, 250)
        if hb is None:
            raise BatchError("%s line %d: bad hemoglobin '%s'" % (path, n, f[5]))
        out.append(Visit(n - 1, day, minute, did, f[4], hb))
    return out


def parse_tests(path, site, visits, text):
    out = []
    n = 1
    for line in body_lines(path, text, TEST_HEADER):
        n += 1
        f = fields_of(path, n, line, 6)
        if n - 1 > len(visits) or f[0] != str(n - 1):
            raise BatchError("%s line %d: no visit '%s'" % (path, n, f[0]))
        results = []
        for m in range(4):
            if f[1 + m] != "R" and f[1 + m] != "N":
                raise BatchError("%s line %d: bad %s result '%s'" % (path, n, MARKERS[m], f[1 + m]))
            results.append(f[1 + m] == "R")
        g = group_of(f[5])
        if g < 0:
            raise BatchError("%s line %d: bad group '%s'" % (path, n, f[5]))
        out.append(Test(n - 1, results, g))
    if len(out) != len(visits):
        raise BatchError("%s: %d results for %d visits" % (path, len(out), len(visits)))
    return out


def parse_orders(path, site, text):
    out = []
    n = 1
    last = 1
    for line in body_lines(path, text, ORDER_HEADER):
        n += 1
        f = fields_of(path, n, line, 7)
        if f[0] != str(n - 1):
            raise BatchError("%s line %d: order '%s' out of order" % (path, n, f[0]))
        day = small_int(f[1], 1, DAYS)
        if day is None or day < last:
            raise BatchError("%s line %d: bad day '%s'" % (path, n, f[1]))
        last = day
        ok = False
        for h in range(site.hospitals):
            if f[2] == hospital_code(site, h):
                ok = True
        if not ok:
            raise BatchError("%s line %d: hospital %s is not at %s" % (path, n, f[2], site.code))
        c = comp_of(f[3])
        if c < 0:
            raise BatchError("%s line %d: bad component '%s'" % (path, n, f[3]))
        g = group_of(f[4])
        if g < 0:
            raise BatchError("%s line %d: bad group '%s'" % (path, n, f[4]))
        units = small_int(f[5], 1, 20)
        if units is None:
            raise BatchError("%s line %d: bad units '%s'" % (path, n, f[5]))
        if f[6] != "ROUTINE" and f[6] != "URGENT":
            raise BatchError("%s line %d: bad priority '%s'" % (path, n, f[6]))
        out.append(Order(n - 1, day, f[2], c, g, units, f[6] == "URGENT"))
    return out


# ------------------------------------------------------------ the week

def new_unit(site, w, comp, group, day):
    w.made += 1
    s = Stock("U%d%06d" % (site.digit, w.made), comp, group, day, day + SHELF[comp])
    w.pending.append(s)
    w.produced[comp] += 1


def eligible(dn, v):
    """The reason a donor may not give today, or -1."""
    d = v.day
    if dn.deferred > 0 and d < dn.deferred:
        return 0
    age = YEAR - dn.born
    if age < 18 or age > 70:
        return 1
    if dn.weight < MIN_WEIGHT:
        return 2
    if v.hb < MIN_HB:
        return 3
    if v.kind == "WB":
        if d - dn.last_wb < WB_GAP or d - dn.last_aph < APH_GAP:
            return 4
    else:
        if d - dn.last_aph < APH_GAP or d - dn.last_wb < APH_AFTER_WB:
            return 4
    return -1


def pick(w, comp, group, any_rh):
    best = None
    best_key = None
    for s in w.stock:
        if s.gone or s.comp != comp or not compatible(comp, s.group, group, any_rh):
            continue
        if comp == 1:
            k = (0 if s.group == group else 1, 1 if rh(s.group) > rh(group) else 0, s.exp, s.uid)
        else:
            k = (0 if s.group == group else 1, reach(comp, s.group), s.exp, s.uid)
        if best is None or k < best_key:
            best = s
            best_key = k
    return best


def close_week(site, donors, units, visits, tests, orders):
    w = Week()
    w.made = len(units)
    w.stock = [Stock(u.uid, u.comp, u.group, u.collected, u.collected + SHELF[u.comp]) for u in units]
    w.pending = []
    w.produced = [0, 0, 0]
    w.expired = [0, 0, 0]
    w.deferrals = [0, 0, 0, 0, 0]
    w.by_day = [0] * DAYS
    w.visits = 0
    w.wb = 0
    w.aph = 0
    w.reactive = [0, 0, 0, 0]
    w.reactive_donations = 0
    w.abo = 0
    w.barred = 0
    w.discarded = 0
    w.buffy = 0
    w.unpooled = 0
    w.pools = 0
    w.aph_units = 0
    w.orders = 0
    w.asked = 0
    w.issued = 0
    w.substituted = 0
    w.override = 0
    w.short = [0, 0, 0]
    w.hosp_asked = [0] * site.hospitals
    w.hosp_issued = [0] * site.hospitals
    w.issues = []
    dmap = {}
    for k in range(len(donors)):
        dmap[donors[k].did] = k
    ds = [Donor(*d.key()) for d in donors]
    vi = 0
    oi = 0
    for day in range(1, DAYS + 1):
        for s in w.stock:
            if not s.gone and s.exp <= day:
                s.gone = True
                w.expired[s.comp] += 1
        w.stock.extend(w.pending)
        w.pending = []
        coats = [0] * 8
        while vi < len(visits) and visits[vi].day == day:
            v = visits[vi]
            t = tests[vi]
            vi += 1
            w.visits += 1
            dn = ds[dmap[v.did]]
            why = eligible(dn, v)
            if why >= 0:
                w.deferrals[why] += 1
                continue
            w.by_day[day - 1] += 1
            if v.kind == "WB":
                w.wb += 1
                dn.last_wb = day
            else:
                w.aph += 1
                dn.last_aph = day
            n_units = 3 if v.kind == "WB" else (2 if dn.weight >= SPLIT_WEIGHT else 1)
            if any(t.results):
                w.reactive_donations += 1
                for m in range(4):
                    if t.results[m]:
                        w.reactive[m] += 1
                dn.deferred = FOREVER
                w.barred += 1
                w.discarded += n_units
                continue
            if t.typed != dn.group:
                w.abo += 1
                w.discarded += n_units
                continue
            if v.kind == "WB":
                new_unit(site, w, 0, dn.group, day)
                new_unit(site, w, 2, dn.group, day)
                coats[dn.group] += 1
                w.buffy += 1
            else:
                for _ in range(n_units):
                    new_unit(site, w, 1, dn.group, day)
                    w.aph_units += 1
        for g in range(8):
            while coats[g] >= POOL:
                coats[g] -= POOL
                new_unit(site, w, 1, g, day)
                w.pools += 1
            w.unpooled += coats[g]
        todays = []
        while oi < len(orders) and orders[oi].day == day:
            todays.append(orders[oi])
            oi += 1
        todays.sort(key=lambda o: (0 if o.urgent else 1, o.ono))
        for o in todays:
            w.orders += 1
            w.asked += o.units
            h = int(o.hospital[len(site.code) + 2:]) - 1
            w.hosp_asked[h] += o.units
            for _ in range(o.units):
                s = pick(w, o.comp, o.group, False)
                if s is None and o.comp == 0 and o.urgent and rh(o.group) == 0:
                    s = pick(w, o.comp, o.group, True)
                    if s is not None:
                        w.override += 1
                if s is None:
                    w.short[o.comp] += 1
                    continue
                s.gone = True
                w.issued += 1
                w.hosp_issued[h] += 1
                if s.group != o.group:
                    w.substituted += 1
                w.issues.append((s.uid, o.ono, day, o.hospital, o.comp, s.group, o.group, s.exp))
    w.stock.extend(w.pending)
    w.pending = []
    return w


# ------------------------------------------------------------ reports

def collection_report(site, w):
    deferred = sum(w.deferrals)
    order = [4, 3, 1, 2, 0]
    parts = ", ".join("%d %s" % (w.deferrals[r], REASONS[r]) for r in order)
    return "%s collections: %s visits, %s donations (%s whole blood, %s apheresis), %s deferred (%s); by day %s" % (
        site.code, group3(w.visits), group3(w.wb + w.aph), group3(w.wb), group3(w.aph), group3(deferred), parts,
        " ".join(str(x) for x in w.by_day))


def testing_report(site, w):
    parts = ", ".join("%s %d" % (MARKERS[m], w.reactive[m]) for m in range(4))
    return "%s testing: %s tested, %d reactive (%s), %d failed ABO confirmation; %s deferred for good, %s discarded" % (
        site.code, group3(w.wb + w.aph), w.reactive_donations, parts, w.abo, plural(w.barred, "donor"),
        plural(w.discarded, "component"))


def component_report(site, w):
    exp = ", ".join("%s %d" % (NAMES[c], w.expired[c]) for c in range(3))
    return "%s components: %s red cells, %s plasma, %s platelet pools from %s buffy coats (%d unpooled), %s apheresis platelets; %s expired (%s)" % (
        site.code, group3(w.produced[0]), group3(w.produced[2]), group3(w.pools), group3(w.buffy), w.unpooled,
        group3(w.aph_units), group3(sum(w.expired)), exp)


def hospital_report(site, w):
    hs = ", ".join("%s %d/%d" % (hospital_code(site, h), w.hosp_issued[h], w.hosp_asked[h]) for h in range(site.hospitals))
    short = ", ".join("%s %d" % (NAMES[c], w.short[c]) for c in range(3))
    return "%s hospitals: %s orders for %s units, %s issued (%d substituted, %d Rh override), %d short (%s); %s" % (
        site.code, group3(w.orders), group3(w.asked), group3(w.issued), w.substituted, w.override, sum(w.short),
        short, hs)


def inventory_report(site, w):
    rbc = [0] * 8
    plt = 0
    ffp = 0
    soon = 0
    for s in w.stock:
        if s.gone:
            continue
        if s.comp == 0:
            rbc[s.group] += 1
        elif s.comp == 1:
            plt += 1
        else:
            ffp += 1
        if s.exp <= DAYS + 3:
            soon += 1
    cells = ", ".join("%s %d" % (GROUPS[g], rbc[g]) for g in range(8))
    return "%s inventory: red cells %s; %d platelets, %s plasma; %d expire within 3 days" % (
        site.code, cells, plt, group3(ffp), soon)


def draw(reserve, site, w):
    asked = w.short[0]
    sent = min(asked, reserve.left)
    reserve.left -= sent
    reserve.log.append("%s,%d,%d,%d" % (site.code, asked, sent, reserve.left))
    return "%s reserve: asked for %d O- red cells, sent %d, %d not covered; %d left in the reserve" % (
        site.code, asked, sent, asked - sent, reserve.left)


# ------------------------------------------------------------ the job

def run():
    nor = Site("NOR", 1, 8101, 2400, 70, 3, 260, 24)
    sou = Site("SOU", 2, 8202, 2900, 85, 4, 300, 30)
    eas = Site("EAS", 3, 8303, 1800, 55, 2, 200, 21)
    wes = Site("WES", 4, 8404, 2100, 60, 3, 220, 22)
    sites = [nor, sou, eas, wes]

    d = [build_donors(s) for s in sites]
    st = [build_stock(s) for s in sites]
    v = [build_visits(sites[i], d[i]) for i in range(4)]
    t = [build_tests(sites[i], v[i], d[i]) for i in range(4)]
    o = [build_orders(s) for s in sites]

    kinds = [("donors", encode_donors, d), ("stock", encode_stock, st), ("visits", encode_visits, v),
             ("tests", encode_tests, t), ("orders", encode_orders, o)]
    for i in range(4):
        for (name, enc, data) in kinds:
            with open("%s_%s.csv" % (sites[i].code, name), "w") as f:
                f.write(enc(data[i]))

    whole = read_text("NOR_visits.csv")
    with open("NOR_visits.bad", "w") as f:
        f.write(damage(whole, 40, 3, "199999"))
    whole2 = read_text("SOU_tests.csv")
    with open("SOU_tests.bad", "w") as f:
        f.write(damage(whole2, 30, 3, "X"))

    texts = []
    for i in range(4):
        row = {}
        for (name, _, _) in kinds:
            row[name] = read_text("%s_%s.csv" % (sites[i].code, name))
        texts.append(row)
    for i in range(4):
        for (name, _, _) in kinds:
            print(file_line("%s %s" % (sites[i].code, name), texts[i][name]))

    pd = [parse_donors("%s_donors.csv" % sites[i].code, sites[i], texts[i]["donors"]) for i in range(4)]
    ps = [parse_stock("%s_stock.csv" % sites[i].code, sites[i], texts[i]["stock"]) for i in range(4)]
    po = [parse_orders("%s_orders.csv" % sites[i].code, sites[i], texts[i]["orders"]) for i in range(4)]
    pv = [parse_visits("%s_visits.csv" % sites[i].code, sites[i], pd[i], texts[i]["visits"]) for i in range(4)]
    pt = [parse_tests("%s_tests.csv" % sites[i].code, sites[i], pv[i], texts[i]["tests"]) for i in range(4)]
    for (path, fn) in [("NOR_visits.bad", lambda x: parse_visits("NOR_visits.bad", nor, pd[0], x)),
                       ("SOU_tests.bad", lambda x: parse_tests("SOU_tests.bad", sou, pv[1], x))]:
        try:
            fn(read_text(path))
            print(path + ": parsed")
        except BatchError as e:
            print(str(e))
    try:
        parse_orders("EAS_orders.csv", wes, texts[2]["orders"])
        print("EAS_orders.csv: parsed")
    except BatchError as e:
        print(str(e))
    same = True
    for i in range(4):
        for (a, b) in [(pd[i], d[i]), (ps[i], st[i]), (pv[i], v[i]), (pt[i], t[i]), (po[i], o[i])]:
            if [x.key() for x in a] != [x.key() for x in b]:
                same = False
    print("parsed files match the built ones: %s" % ("true" if same else "false"))

    w = [close_week(sites[i], pd[i], ps[i], pv[i], pt[i], po[i]) for i in range(4)]

    for i in range(4):
        with open("%s_issues.csv" % sites[i].code, "w") as f:
            f.write(encode_issues(w[i]))
        with open("%s_inventory.csv" % sites[i].code, "w") as f:
            f.write(encode_inventory(w[i]))
    for i in range(4):
        print(file_line("%s issues" % sites[i].code, read_text("%s_issues.csv" % sites[i].code)))
        print(file_line("%s inventory" % sites[i].code, read_text("%s_inventory.csv" % sites[i].code)))

    for fn in [collection_report, testing_report, component_report, hospital_report, inventory_report]:
        for i in range(4):
            print(fn(sites[i], w[i]))

    reserve = Reserve()
    order = [0, 1, 2, 3]
    if os.environ.get("BLOOD_REVERSE") == "1":
        order = [3, 2, 1, 0]
    for i in order:
        print(draw(reserve, sites[i], w[i]))
    with open("reserve.log", "w") as f:
        f.write("\n".join(reserve.log) + "\n")
    back = read_text("reserve.log")
    print("reserve.log: %s entries, %s bytes, fnv %08x" % (group3(len(back.split("\n")) - 1), group3(len(back)),
                                                          text_sum(back)))

    for i in range(4):
        for name in ["donors", "stock", "visits", "tests", "orders", "issues", "inventory"]:
            os.remove("%s_%s.csv" % (sites[i].code, name))
    for p in ["NOR_visits.bad", "SOU_tests.bad", "reserve.log"]:
        os.remove(p)


def main():
    try:
        run()
    except BatchError as e:
        print("blood_bank failed: %s" % e)


main()
