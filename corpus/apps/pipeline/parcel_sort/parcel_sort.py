"""pipeline/parcel_sort, the Python mirror: a parcel carrier's night sort
over four depots, from shipper manifests and scan logs to the join that
finds duplicate scans and parcels nobody manifested, rating, chute
assignment, reports, and one shared linehaul that loads the trailers to
the national hub. It wrote expected.out.

Run from this directory: python3 -I -B parcel_sort.py > expected.out"""
import os

MASK32 = 4294967295
PARCELS = 2400
UNMANIFESTED = 24
SHIFT = 28800
CUTOFF = 27000
BAG = 40
SERVICES = ["G", "E", "O"]
SERVICE_NAMES = ["ground", "express", "overnight"]
# Shipper accounts and their contract discounts on transport, in basis
# points.
SHIPPERS = ["SH01", "SH02", "SH03", "SH04", "SH05", "SH06", "SH07", "SH08", "SH09", "SH10"]
DISCOUNT = [1800, 1200, 900, 1500, 600, 1000, 0, 300, 700, 0]
# The three-digit postcode prefixes parcels go to.
DESTS = [100, 104, 112, 191, 200, 212, 303, 331, 352, 402, 441, 454, 462, 482, 532, 554, 606, 631, 750, 770, 802, 850, 900, 941, 980, 995]
# Per service, by zone 2 to 8: the charge in cents for the first 500 g
# and for each 500 g after it.
BASE = [
    [895, 945, 1010, 1085, 1160, 1240, 1330],
    [1450, 1590, 1760, 1940, 2130, 2330, 2540],
    [2790, 3120, 3490, 3880, 4290, 4720, 5170],
]
PER = [
    [42, 51, 63, 77, 92, 108, 125],
    [85, 102, 121, 143, 166, 190, 215],
    [160, 189, 221, 255, 291, 329, 369],
]
FUEL_BP = 1425
OVERSIZE = 9500
HAZMAT = 3500
SIGNATURE = 650
INSURED_FREE = 10000
INSURED_STEP = 10000
INSURED_RATE = 110
# A linehaul trailer to the hub: its payload in grams and its cube in
# cubic centimetres.
TRAILER_G = 12500000
TRAILER_CM3 = 70000000


class BatchError(Exception):
    pass


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
        out = "," + s[-3:] + out
        s = s[:-3]
    return s + out


def money(c):
    sign = "-" if c < 0 else ""
    c = abs(c)
    return f"{sign}${group3(c // 100)}.{c % 100:02}"


def dollars(c):
    return f"{c // 100}.{c % 100:02}"


def kg(g):
    return f"{group3(g // 1000)}.{g % 1000 // 100} kg"


def m3(cm3):
    q = cm3 // 100000
    return f"{q // 10}.{q % 10} m3"


def ceil_div(a, b):
    return (a + b - 1) // b


def is_digits(t):
    return len(t) > 0 and all("0" <= ch <= "9" for ch in t)


def cents_of(t):
    dot = t.find(".")
    if dot < 1 or len(t) - dot != 3:
        return None
    a, b = t[:dot], t[dot + 1:]
    if not (is_digits(a) and is_digits(b)):
        return None
    return int(a) * 100 + int(b)


# The shift runs from 20:00 on 23 October to 04:00 on the 24th; a time is
# the seconds since it began.
def stamp(t):
    h = 20 + t // 3600
    day = 23
    if h >= 24:
        h -= 24
        day = 24
    return f"2026-10-{day} {h:02}:{t % 3600 // 60:02}:{t % 60:02}"


def clock(t):
    return f"{(20 + t // 3600) % 24:02}:{t % 3600 // 60:02}"


def secs_of(t):
    if len(t) != 19 or t[4] != "-" or t[7] != "-" or t[10] != " " or t[13] != ":" or t[16] != ":":
        return None
    parts = [t[0:4], t[5:7], t[8:10], t[11:13], t[14:16], t[17:19]]
    if not all(is_digits(p) for p in parts):
        return None
    y, m, d, hh, mm, ss = (int(p) for p in parts)
    if y != 2026 or m != 10 or mm > 59 or ss > 59:
        return None
    if d == 23 and 20 <= hh <= 23:
        return (hh - 20) * 3600 + mm * 60 + ss
    if d == 24 and hh < 4:
        return (hh + 4) * 3600 + mm * 60 + ss
    return None


class Depot:
    def __init__(self, name, code, prefix, seed, offset):
        self.name = name
        self.code = code
        self.prefix = prefix
        self.seed = seed
        self.offset = offset


class Parcel:
    def __init__(self, pid, shipper, service, grams, l, w, h, post, value, flags):
        self.pid = pid
        self.shipper = shipper
        self.service = service
        self.grams = grams
        self.l = l
        self.w = w
        self.h = h
        self.post = post
        self.value = value
        self.flags = flags


class Scan:
    def __init__(self, pid, scanner, t, grams):
        self.pid = pid
        self.scanner = scanner
        self.t = t
        self.grams = grams


PARCEL_KEYS = ["pid", "shipper", "service", "grams", "l", "w", "h", "post", "value", "flags"]
SCAN_KEYS = ["pid", "scanner", "t", "grams"]


# ------------------------------------------------------------ building

def build_manifest(dp):
    out = []
    for i in range(PARCELS):
        def r(salt):
            return mix(dp.seed, i, salt)
        pid = f"{dp.code}{1000000 + i * 7:07}"
        sr = r(1) % 100
        service = 0 if sr < 68 else (1 if sr < 88 else 2)
        shipper = (min(r(2) % 10, r(3) % 10) + dp.offset) % 10
        kind = r(4) % 10
        if kind < 6:
            l, w, h = 15 + r(5) % 30, 10 + r(6) % 25, 5 + r(7) % 20
        elif kind < 9:
            l, w, h = 30 + r(5) % 40, 25 + r(6) % 30, 15 + r(7) % 30
        else:
            l, w, h = 60 + r(5) % 90, 40 + r(6) % 50, 30 + r(7) % 50
        dens = 40 + r(8) % 360
        grams = max(100, min(68000, l * w * h * dens // 1000))
        if r(9) % 150 == 0:
            prefix = r(10) % 5
        else:
            prefix = DESTS[r(10) % len(DESTS)]
        post = f"{prefix:03}{r(11) % 100:02}"
        if r(13) % 4 == 0:
            value = 10000 + r(12) % 90000
        else:
            value = 500 + r(12) % 9500
        flags = ""
        if r(14) % 60 == 0:
            flags += "H"
        if r(15) % 8 == 0:
            flags += "F"
        if value > 50000 or r(16) % 20 == 0:
            flags += "S"
        if flags == "":
            flags = "-"
        out.append(Parcel(pid, SHIPPERS[shipper], SERVICES[service], grams, l, w, h, post, value, flags))
    return out


def build_scans(dp, man):
    out = []
    for i, p in enumerate(man):
        def r(salt):
            return mix(dp.seed, i, salt)
        if r(20) % 80 == 0:
            continue
        t = (r(21) % SHIFT + r(22) % SHIFT) // 2
        scanner = f"S{1 + r(23) % 6}"
        if r(24) % 20 == 0:
            g = p.grams * (130 + r(25) % 70) // 100
        else:
            g = p.grams * (97 + r(25) % 7) // 100
        g = max(100, g)
        out.append(Scan(p.pid, scanner, t, g))
        if r(26) % 40 == 0:
            out.append(Scan(p.pid, f"S{1 + r(27) % 6}", min(SHIFT - 1, t + 5 + r(28) % 600), g))
    for k in range(UNMANIFESTED):
        def r(salt):
            return mix(dp.seed, k, salt)
        pid = f"{dp.code}{9000000 + k * 37:07}"
        out.append(Scan(pid, f"S{1 + r(31) % 6}", r(30) % SHIFT, 200 + r(32) % 20000))
    out.sort(key=lambda s: (s.scanner, s.t, s.pid))
    return out


def manifest_header():
    return "parcel,shipper,service,grams,length,width,height,postcode,value,flags"


def scan_header():
    return "parcel,scanner,time,grams"


def encode_manifest(man):
    lines = [manifest_header()]
    for p in man:
        lines.append(f"{p.pid},{p.shipper},{p.service},{p.grams},{p.l},{p.w},{p.h},{p.post},{dollars(p.value)},{p.flags}")
    return "\n".join(lines) + "\n"


def encode_scans(scans):
    lines = [scan_header()]
    for s in scans:
        lines.append(f"{s.pid},{s.scanner},{stamp(s.t)},{s.grams}")
    return "\n".join(lines) + "\n"


def read_text(path):
    try:
        with open(path, "r", newline="") as f:
            return f.read()
    except OSError:
        raise BatchError(f"cannot read {path}")


def write_text(path, text):
    with open(path, "w", newline="") as f:
        f.write(text)


def damage(text, n, field, value):
    lines = text.split("\n")
    f = lines[n].split(",")
    f[field] = value
    lines[n] = ",".join(f)
    return "\n".join(lines)


# ------------------------------------------------------------ parsing

def pid_ok(t, code):
    return len(t) == 8 and t[0] == code and is_digits(t[1:])


def flags_ok(t):
    if t == "-":
        return True
    if t == "":
        return False
    at = 0
    for ch in t:
        k = "HFS".find(ch, at)
        if k < 0:
            return False
        at = k + 1
    return True


def body_lines(path, text, header):
    lines = text.split("\n")
    if len(lines) < 2 or lines[-1] != "":
        raise BatchError(f"{path}: no final newline")
    if lines[0] != header:
        raise BatchError(f"{path} line 1: bad header")
    return lines[1:-1]


def parse_manifest(path, dp, text):
    out = []
    for k, line in enumerate(body_lines(path, text, manifest_header())):
        def bad(what):
            return BatchError(f"{path} line {k + 2}: {what}")
        f = line.split(",")
        if len(f) != 10:
            raise bad(f"expected 10 fields, found {len(f)}")
        if not pid_ok(f[0], dp.code):
            raise bad(f"bad parcel id '{f[0]}'")
        if f[1] not in SHIPPERS:
            raise bad(f"unknown shipper '{f[1]}'")
        if f[2] not in SERVICES:
            raise bad(f"bad service '{f[2]}'")
        if not is_digits(f[3]) or len(f[3]) > 5 or not 1 <= int(f[3]) <= 68000:
            raise bad(f"bad weight '{f[3]}'")
        dims = []
        for d in f[4:7]:
            if not is_digits(d) or len(d) > 3 or not 1 <= int(d) <= 274:
                raise bad(f"bad dimension '{d}'")
            dims.append(int(d))
        if len(f[7]) != 5 or not is_digits(f[7]):
            raise bad(f"bad postcode '{f[7]}'")
        value = cents_of(f[8])
        if value is None:
            raise bad(f"bad value '{f[8]}'")
        if not flags_ok(f[9]):
            raise bad(f"bad flags '{f[9]}'")
        out.append(Parcel(f[0], f[1], f[2], int(f[3]), dims[0], dims[1], dims[2], f[7], value, f[9]))
    return out


def parse_scans(path, dp, text):
    out = []
    for k, line in enumerate(body_lines(path, text, scan_header())):
        def bad(what):
            return BatchError(f"{path} line {k + 2}: {what}")
        f = line.split(",")
        if len(f) != 4:
            raise bad(f"expected 4 fields, found {len(f)}")
        if not pid_ok(f[0], dp.code):
            raise bad(f"bad parcel id '{f[0]}'")
        if len(f[1]) != 2 or f[1][0] != "S" or not "1" <= f[1][1] <= "6":
            raise bad(f"bad scanner '{f[1]}'")
        t = secs_of(f[2])
        if t is None:
            raise bad(f"bad time '{f[2]}'")
        if not is_digits(f[3]) or len(f[3]) > 6 or int(f[3]) < 1:
            raise bad(f"bad weight '{f[3]}'")
        out.append(Scan(f[0], f[1], t, int(f[3])))
    return out


def same(a, b, keys):
    return len(a) == len(b) and all(all(getattr(x, k) == getattr(y, k) for k in keys) for x, y in zip(a, b))


# ------------------------------------------------------------ the join

class Joined:
    def __init__(self):
        self.arrivals = []      # (manifest index, scan), in time order
        self.unmanifested = []  # scans, in time order
        self.dups = 0
        self.missing = []       # manifest indexes never scanned


def join(man, scans):
    jn = Joined()
    index = {}
    for i, p in enumerate(man):
        index[p.pid] = i
    order = sorted(range(len(scans)), key=lambda i: (scans[i].t, i))
    seen = set()
    for i in order:
        s = scans[i]
        if s.pid in seen:
            jn.dups += 1
            continue
        seen.add(s.pid)
        if s.pid in index:
            jn.arrivals.append((index[s.pid], s))
        else:
            jn.unmanifested.append(s)
    for i, p in enumerate(man):
        if p.pid not in seen:
            jn.missing.append(i)
    return jn


# ------------------------------------------------------------ rating

class Rated:
    def __init__(self):
        self.exception = ""
        self.zone = 0
        self.units = 0
        self.transport = 0
        self.fuel = 0
        self.surcharge = 0
        self.total = 0
        self.adjust = 0
        self.reweighed = False
        self.dim = False
        self.oversize = False
        self.chute = ""


def transport_of(dp, p, weight, zone):
    s = SERVICES.index(p.service)
    dim = ceil_div(p.l * p.w * p.h, 5)
    units = ceil_div(max(weight, dim), 500)
    t = BASE[s][zone - 2] + PER[s][zone - 2] * (units - 1)
    t -= t * DISCOUNT[SHIPPERS.index(p.shipper)] // 10000
    return t, units


def rate_one(dp, p, s):
    rt = Rated()
    dest = int(p.post[:3])
    hazmat = "H" in p.flags
    if dest < 5:
        rt.exception = "bad address"
        rt.chute = "EX"
        return rt
    if hazmat and p.service == "O":
        rt.exception = "hazmat by air"
        rt.chute = "EX"
        return rt
    zone = min(8, 2 + abs(dest - dp.prefix) // 120)
    weight = max(p.grams, s.grams)
    t, units = transport_of(dp, p, weight, zone)
    longest = max(p.l, p.w, p.h)
    girth = longest + 2 * (p.l + p.w + p.h - longest)
    rt.oversize = longest > 120 or girth > 300
    sur = 0
    if rt.oversize:
        sur += OVERSIZE
    if hazmat:
        sur += HAZMAT
    if "S" in p.flags:
        sur += SIGNATURE
    if p.value > INSURED_FREE:
        sur += ceil_div(p.value - INSURED_FREE, INSURED_STEP) * INSURED_RATE
    rt.zone = zone
    rt.units = units
    rt.transport = t
    rt.fuel = (t * FUEL_BP + 5000) // 10000
    rt.surcharge = sur
    rt.total = t + rt.fuel + sur
    rt.dim = ceil_div(p.l * p.w * p.h, 5) > weight
    if s.grams > p.grams * 110 // 100 + 200:
        rt.reweighed = True
        t0, _ = transport_of(dp, p, p.grams, zone)
        rt.adjust = (t + rt.fuel) - (t0 + (t0 * FUEL_BP + 5000) // 10000)
    if rt.oversize:
        rt.chute = "OS"
    elif hazmat:
        rt.chute = "HZ"
    elif p.service == "O":
        rt.chute = f"A{zone}"
    elif p.service == "E":
        rt.chute = f"X{zone}"
    else:
        rt.chute = f"G{dest // 100}"
    return rt


def rate(dp, man, jn):
    return [rate_one(dp, man[i], s) for i, s in jn.arrivals]


# ------------------------------------------------------------ reports

def volume_report(dp, man, scans, jn):
    hours = [0] * 8
    for _, s in jn.arrivals:
        hours[s.t // 3600] += 1
    for s in jn.unmanifested:
        hours[s.t // 3600] += 1
    best = 0
    for h in range(8):
        if hours[h] > hours[best]:
            best = h
    return (f"{dp.name} volume: {group3(len(man))} manifested, {group3(len(jn.arrivals))} arrived, "
            f"{len(jn.missing)} not arrived, {len(jn.unmanifested)} unmanifested, {jn.dups} duplicate scans "
            f"of {group3(len(scans))}; busiest hour {clock(best * 3600)} with {group3(hours[best])} scans")


def revenue_report(dp, man, jn, rated):
    total = 0
    by_service = [0, 0, 0]
    fuel = 0
    sur = 0
    n = 0
    by_shipper = [0] * len(SHIPPERS)
    for (i, _), rt in zip(jn.arrivals, rated):
        if rt.exception:
            continue
        p = man[i]
        n += 1
        total += rt.total
        by_service[SERVICES.index(p.service)] += rt.total
        fuel += rt.fuel
        sur += rt.surcharge
        by_shipper[SHIPPERS.index(p.shipper)] += rt.total
    top = 0
    for k in range(len(SHIPPERS)):
        if by_shipper[k] > by_shipper[top]:
            top = k
    avg = total // n if n else 0
    return (f"{dp.name} revenue: {money(total)} on {group3(n)} parcels (ground {money(by_service[0])}, "
            f"express {money(by_service[1])}, overnight {money(by_service[2])}); fuel {money(fuel)}, "
            f"surcharges {money(sur)}; average {money(avg)}; top shipper {SHIPPERS[top]} {money(by_shipper[top])}")


def weight_report(dp, man, jn, rated):
    rew = 0
    adj = 0
    over = 0
    dims = 0
    big = 0
    heavy = -1
    for k, ((i, s), rt) in enumerate(zip(jn.arrivals, rated)):
        if rt.exception:
            continue
        p = man[i]
        if rt.reweighed:
            rew += 1
            adj += rt.adjust
            over += s.grams - p.grams
        if rt.dim:
            dims += 1
        if rt.oversize:
            big += 1
        if heavy < 0 or s.grams > jn.arrivals[heavy][1].grams:
            heavy = k
    hs = jn.arrivals[heavy][1]
    return (f"{dp.name} weights: {rew} reweighed for {money(adj)} ({kg(over)} over declared); "
            f"{group3(dims)} billed on dim weight; {big} oversize; heaviest {hs.pid} {kg(hs.grams)}")


def chute_counts(jn, rated):
    names = []
    counts = []
    for rt in rated:
        if rt.chute in names:
            counts[names.index(rt.chute)] += 1
        else:
            names.append(rt.chute)
            counts.append(1)
    if jn.unmanifested:
        if "EX" in names:
            counts[names.index("EX")] += len(jn.unmanifested)
        else:
            names.append("EX")
            counts.append(len(jn.unmanifested))
    return names, counts


def sort_report(dp, jn, rated):
    names, counts = chute_counts(jn, rated)
    order = sorted(range(len(names)), key=lambda k: (-counts[k], names[k]))
    bags = sum(ceil_div(c, BAG) for c in counts)

    def count_of(name):
        return counts[names.index(name)] if name in names else 0
    busiest = ", ".join(f"{names[k]} {counts[k]}" for k in order[:3])
    return (f"{dp.name} sort: {len(names)} chutes, {bags} bags; busiest {busiest}; "
            f"EX {count_of('EX')}, OS {count_of('OS')}, HZ {count_of('HZ')}")


def exception_report(dp, man, jn, rated):
    addr = 0
    air = 0
    missed = 0
    first = -1
    for (i, s), rt in zip(jn.arrivals, rated):
        if rt.exception == "bad address":
            addr += 1
            if first < 0:
                first = i
        elif rt.exception == "hazmat by air":
            air += 1
        elif s.t >= CUTOFF:
            missed += 1
    tail = f"first bad address {man[first].pid} '{man[first].post}'" if first >= 0 else "no bad address"
    return (f"{dp.name} exceptions: {addr} bad address, {air} hazmat by air, {len(jn.unmanifested)} unmanifested, "
            f"{missed} missed the {clock(CUTOFF)} cutoff, {len(jn.missing)} not arrived; {tail}")


# ------------------------------------------------------------ the linehaul

class Linehaul:
    def __init__(self):
        self.number = 0
        self.parcels = 0
        self.grams = 0
        self.cm3 = 0
        self.depots = ""
        self.log = []


def close_trailer(lh, why):
    lh.log.append(f"TR-{lh.number:04} {group3(lh.parcels)} parcels, {kg(lh.grams)}, {m3(lh.cm3)}, from {lh.depots}; {why}")
    lh.parcels = 0
    lh.grams = 0
    lh.cm3 = 0
    lh.depots = ""


def load(lh, dp, man, jn, rated):
    first = 0
    count = 0
    air = 0
    hz = 0
    held = 0
    for (i, s), rt in zip(jn.arrivals, rated):
        if rt.exception:
            continue
        if s.t >= CUTOFF:
            held += 1
            continue
        if rt.chute[0] == "A":
            air += 1
            continue
        if rt.chute == "HZ":
            hz += 1
            continue
        p = man[i]
        v = p.l * p.w * p.h
        if lh.number == 0:
            lh.number = 1
        elif lh.grams + s.grams > TRAILER_G:
            close_trailer(lh, "weight full")
            lh.number += 1
        elif lh.cm3 + v > TRAILER_CM3:
            close_trailer(lh, "cube full")
            lh.number += 1
        if first == 0:
            first = lh.number
        lh.parcels += 1
        lh.grams += s.grams
        lh.cm3 += v
        if dp.code not in lh.depots:
            lh.depots = dp.code if lh.depots == "" else lh.depots + "+" + dp.code
        count += 1
    return (f"{dp.name} linehaul: {group3(count)} parcels onto TR-{first:04} to TR-{lh.number:04}; "
            f"{air} to the airport, {hz} on the hazmat truck, {held} held for tomorrow")


# ------------------------------------------------------------ the run

def run():
    depots = [
        Depot("akron", "A", 443, 17, 0),
        Depot("boise", "B", 837, 31, 3),
        Depot("camden", "C", 81, 47, 6),
        Depot("dayton", "D", 454, 61, 8),
    ]
    mans = [build_manifest(d) for d in depots]
    scans = [build_scans(d, m) for d, m in zip(depots, mans)]
    for d, m in zip(depots, mans):
        write_text(f"{d.name}_manifest.csv", encode_manifest(m))
    for d, s in zip(depots, scans):
        write_text(f"{d.name}_scans.csv", encode_scans(s))

    whole = read_text("boise_scans.csv")
    write_text("boise_scans.bad", damage(whole, 1733, 2, "2026-10-24 04:10:00"))

    tm = [read_text(f"{d.name}_manifest.csv") for d in depots]
    ts = [read_text(f"{d.name}_scans.csv") for d in depots]
    for d, a, b in zip(depots, tm, ts):
        print(f"{d.name}: manifest {group3(len(a))} bytes, fnv {text_sum(a):08x}; scans {group3(len(b))} bytes, fnv {text_sum(b):08x}")

    gm = [parse_manifest(f"{d.name}_manifest.csv", d, t) for d, t in zip(depots, tm)]
    gs = [parse_scans(f"{d.name}_scans.csv", d, t) for d, t in zip(depots, ts)]
    for path, dp, text, scan in [("boise_scans.bad", depots[1], read_text("boise_scans.bad"), True), ("dayton_manifest.csv", depots[2], tm[3], False)]:
        try:
            if scan:
                parse_scans(path, dp, text)
            else:
                parse_manifest(path, dp, text)
            print(f"{path}: parsed")
        except BatchError as e:
            print(e)
    ok = all(same(a, b, PARCEL_KEYS) for a, b in zip(gm, mans)) and all(same(a, b, SCAN_KEYS) for a, b in zip(gs, scans))
    print(f"parsed files match the built ones: {'true' if ok else 'false'}")

    jn = [join(m, s) for m, s in zip(gm, gs)]
    rt = [rate(d, m, j) for d, m, j in zip(depots, gm, jn)]

    lines = []
    for d, m, s, j in zip(depots, gm, gs, jn):
        lines.append(volume_report(d, m, s, j))
    for d, m, j, r in zip(depots, gm, jn, rt):
        lines.append(revenue_report(d, m, j, r))
    for d, m, j, r in zip(depots, gm, jn, rt):
        lines.append(weight_report(d, m, j, r))
    for d, j, r in zip(depots, jn, rt):
        lines.append(sort_report(d, j, r))
    for d, m, j, r in zip(depots, gm, jn, rt):
        lines.append(exception_report(d, m, j, r))
    for line in lines:
        print(line)

    lh = Linehaul()
    for d, m, j, r in zip(depots, gm, jn, rt):
        print(load(lh, d, m, j, r))
    close_trailer(lh, "last out")
    for line in lh.log:
        print(line)
    write_text("trailers.log", "\n".join(lh.log) + "\n")
    back = read_text("trailers.log")
    rows = len(back.split("\n")) - 1
    print(f"trailers.log: {rows} trailers, {group3(len(back))} bytes, fnv {text_sum(back):08x}")

    for d in depots:
        os.remove(f"{d.name}_manifest.csv")
        os.remove(f"{d.name}_scans.csv")
    os.remove("boise_scans.bad")
    os.remove("trailers.log")


if __name__ == "__main__":
    try:
        run()
    except BatchError as e:
        print(f"parcel_sort failed: {e}")
