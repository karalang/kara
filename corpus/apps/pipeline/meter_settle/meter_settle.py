"""pipeline/meter_settle, the Python mirror: a weekly smart-meter billing
and wholesale settlement run over four retail regions. It wrote
expected.out.

Run from this directory: python3 -I -B meter_settle.py > expected.out"""
import os

MASK32 = 4294967295
METERS = 96
DAYS = 7
SLOTS = 48
WEEK = DAYS * SLOTS
DAY_NAMES = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"]
KINDS = ["res", "com", "ind"]
# Per kind: the bound on a meter's base half-hour load (Wh) and its floor,
# the time-of-use rates (hundredths of a cent per kWh: peak, shoulder,
# off-peak), the export credit, the daily supply charge (hundredths of a
# cent) and the demand charge (hundredths of a cent per kW of the week's
# highest half hour).
BASE_LO = [150, 800, 4000]
BASE_SPAN = [300, 2200, 8000]
RATE_PEAK = [3450, 2980, 2410]
RATE_SHOULDER = [2280, 2110, 1720]
RATE_OFF = [1390, 1250, 1080]
EXPORT_CREDIT = [620, 540, 480]
SUPPLY = [9800, 24500, 61000]
DEMAND = [0, 26500, 19800]
# The two residential alternatives: one flat rate, and a demand tariff
# (lower energy rates plus a charge on the highest half hour from 16:00
# to 21:00 on weekdays).
FLAT_RATE = 2300
DEMAND_TARIFF_ENERGY = 1640
DEMAND_TARIFF_KW = 50000
GST = 10
# The wholesale portfolio: kWh available in every half hour from each
# tranche and its price (hundredths of a cent per kWh), cheapest first;
# beyond the last tranche energy comes at the scarcity price.
TRANCHES = ["base", "mid", "peaker"]
TRANCHE_KWH = [260, 190, 150]
TRANCHE_PRICE = [450, 720, 1600]
SCARCITY = 9000


def tdiv(a, b):
    q = abs(a) // abs(b)
    return q if (a < 0) == (b < 0) else -q


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


def money(hc):
    # hundredths of a cent as dollars and cents, truncated
    cents = tdiv(hc, 100)
    sign = "-" if cents < 0 else ""
    c = abs(cents)
    return "%s$%s.%02d" % (sign, group3(c // 100), c % 100)


def kwh(wh):
    return group3(tdiv(wh, 1000))


def pct(a, b):
    if b == 0:
        return "0.0%"
    q = tdiv(a * 1000, b)
    return "%d.%d%%" % (q // 10, q % 10)


def when(k):
    d = k // SLOTS
    s = k % SLOTS
    return "%s %02d:%02d" % (DAY_NAMES[d], s // 2, (s % 2) * 30)


class Region:
    def __init__(self, name, code, seed, mix_pct, solar_pct):
        self.name = name
        self.code = code
        self.seed = seed
        self.mix_pct = mix_pct      # res, com, ind shares in percent
        self.solar_pct = solar_pct  # share of residential meters with solar


class Meter:
    def __init__(self, mid, kind, solar, reads):
        self.mid = mid
        self.kind = kind
        self.solar = solar
        self.reads = reads          # WEEK half hours of net Wh, None if missing


class Bill:
    def __init__(self, mid, kind, wh, export, total, best_alt, alt_saving, peak_w):
        self.mid = mid
        self.kind = kind
        self.wh = wh                # imported Wh by band: peak, shoulder, off-peak
        self.export = export
        self.total = total          # hundredths of a cent, with GST
        self.best_alt = best_alt    # -1, or 0 flat / 1 demand tariff for a cheaper residential option
        self.alt_saving = alt_saving
        self.peak_w = peak_w


class Market:
    def __init__(self):
        self.left = [[TRANCHE_KWH[t] * 1000 for t in range(len(TRANCHES))] for _ in range(WEEK)]
        self.tickets = 0
        self.log = ""


class BadData(Exception):
    pass


# ------------------------------------------------------------ the data

def band(k):
    d = k // SLOTS
    s = k % SLOTS
    if d < 5:
        if 32 <= s <= 41:
            return 0
        if 14 <= s <= 31 or 42 <= s <= 43:
            return 1
        return 2
    if 14 <= s <= 43:
        return 1
    return 2


def shape(kind, k):
    d = k // SLOTS
    s = k % SLOTS
    weekday = d < 5
    if kind == 0:
        if s < 12:
            return 40
        if s < 18:
            return 120 if weekday else 80
        if s < 34:
            return 70 if weekday else 105
        if s < 43:
            return 160
        return 90
    if kind == 1:
        if not weekday:
            return 25 if d == 6 else 45
        if 16 <= s <= 35:
            return 150
        if 14 <= s <= 37:
            return 90
        return 30
    if not weekday:
        return 60
    if s in (12, 28, 44):
        return 80
    return 100


def sun(s):
    d = abs(2 * s - 51)
    return max(0, 24 - d)


def build_meters(rg):
    out = []
    day_temp = [90 + mix(rg.seed, d, 9) % 31 for d in range(DAYS)]
    day_cloud = [30 + mix(rg.seed, d, 10) % 71 for d in range(DAYS)]
    for i in range(METERS):
        h = mix(rg.seed, i, 1)
        r = h % 100
        kind = 0 if r < rg.mix_pct[0] else (1 if r < rg.mix_pct[0] + rg.mix_pct[1] else 2)
        solar = 1 if kind == 0 and (h >> 8) % 100 < rg.solar_pct else 0
        base = BASE_LO[kind] + (h >> 12) % BASE_SPAN[kind]
        panel = 600 + (h >> 20) % 1200
        reads = []
        for k in range(WEEK):
            hk = mix(rg.seed, i * WEEK + k, 2)
            if hk % 173 == 0:
                reads.append(None)
                continue
            noise = (hk >> 5) % 31 - 15
            wh = tdiv(base * shape(kind, k) * (100 + noise), 10000)
            if kind == 0:
                wh = tdiv(wh * day_temp[k // SLOTS], 100)
            if solar:
                wh -= tdiv(panel * sun(k % SLOTS) * day_cloud[k // SLOTS], 2400)
            reads.append(wh)
        out.append(Meter("%s-%05d" % (rg.code, i + 1), KINDS[kind], solar, reads))
    return out


def header():
    return "meter,kind,solar,day," + ",".join("h%02d%02d" % (s // 2, (s % 2) * 30) for s in range(SLOTS))


def encode_meters(meters):
    out = header() + "\n"
    for m in meters:
        for d in range(DAYS):
            vals = []
            for s in range(SLOTS):
                x = m.reads[d * SLOTS + s]
                vals.append("" if x is None else str(x))
            out += "%s,%s,%d,%d,%s\n" % (m.mid, m.kind, m.solar, d + 1, ",".join(vals))
    return out


def read_text(path):
    try:
        with open(path, encoding="utf-8", newline="") as f:
            return f.read()
    except OSError:
        raise BadData("could not read " + path)


def write_text(path, text):
    with open(path, "w", encoding="utf-8", newline="") as f:
        f.write(text)


def damage(text, n, field, value):
    lines = text.split("\n")
    f = lines[n].split(",")
    f[field] = value
    lines[n] = ",".join(f)
    return "\n".join(lines)


def is_digits(t):
    return t != "" and all("0" <= c <= "9" for c in t)


def read_field(path, n, s, t, solar):
    if t == "":
        return None
    neg = t.startswith("-")
    body = t[1:] if neg else t
    if not is_digits(body):
        raise BadData("%s line %d: bad read at %s '%s'" % (path, n, "%02d:%02d" % (s // 2, (s % 2) * 30), t))
    if neg and not solar:
        raise BadData("%s line %d: export at %02d:%02d on a meter without solar" % (path, n, s // 2, (s % 2) * 30))
    v = int(body)
    return -v if neg else v


def parse_meters(path, rg, text):
    lines = text.split("\n")
    if lines[0] != header():
        raise BadData(path + ": bad header")
    out = []
    cur = None
    for k in range(1, len(lines)):
        line = lines[k]
        if line == "":
            continue
        n = k + 1
        f = line.split(",")
        if len(f) != 4 + SLOTS:
            raise BadData("%s line %d: %d fields" % (path, n, len(f)))
        mid = f[0]
        if len(mid) != 8 or not mid.startswith(rg.code + "-") or not is_digits(mid[3:8]):
            raise BadData("%s line %d: bad meter '%s'" % (path, n, mid))
        if f[1] not in KINDS:
            raise BadData("%s line %d: bad kind '%s'" % (path, n, f[1]))
        if f[2] not in ("0", "1"):
            raise BadData("%s line %d: bad solar flag '%s'" % (path, n, f[2]))
        if not is_digits(f[3]):
            raise BadData("%s line %d: bad day '%s'" % (path, n, f[3]))
        day = int(f[3])
        solar = 1 if f[2] == "1" else 0
        if day == 1:
            cur = Meter(mid, f[1], solar, [])
            out.append(cur)
        elif cur is None or cur.mid != mid or len(cur.reads) != (day - 1) * SLOTS:
            raise BadData("%s line %d: day %d of %s out of order" % (path, n, day, mid))
        for s in range(SLOTS):
            cur.reads.append(read_field(path, n, s, f[4 + s], solar))
    for m in out:
        if len(m.reads) != WEEK:
            raise BadData("%s: %s has %d of %d reads" % (path, m.mid, len(m.reads), WEEK))
    return out


def same_meters(a, b):
    if len(a) != len(b):
        return False
    for x, y in zip(a, b):
        if x.mid != y.mid or x.kind != y.kind or x.solar != y.solar or x.reads != y.reads:
            return False
    return True


# ------------------------------------------------------------ estimation

def estimate(meters):
    # Fill each gap by straight-line interpolation between the reads
    # either side (truncated), or by the nearest read at the week's ends.
    out = []
    for m in meters:
        reads = list(m.reads)
        k = 0
        filled = 0
        while k < WEEK:
            if reads[k] is not None:
                k += 1
                continue
            j = k
            while j < WEEK and reads[j] is None:
                j += 1
            gap = j - k
            prev = reads[k - 1] if k > 0 else None
            nxt = reads[j] if j < WEEK else None
            for t in range(gap):
                if prev is None:
                    v = nxt if nxt is not None else 0
                elif nxt is None:
                    v = prev
                else:
                    v = prev + tdiv((nxt - prev) * (t + 1), gap + 1)
                reads[k + t] = v
                filled += 1
            k = j
        out.append((Meter(m.mid, m.kind, m.solar, reads), filled))
    return out


# ------------------------------------------------------------ billing

def bill_one(m):
    kind = KINDS.index(m.kind)
    wh = [0, 0, 0]
    export = 0
    peak_wh = 0
    window_peak = 0
    for k in range(WEEK):
        x = m.reads[k]
        if x < 0:
            export -= x
            continue
        b = band(k)
        wh[b] += x
        if x > peak_wh:
            peak_wh = x
        if b == 0 and x > window_peak:
            window_peak = x
    energy = (tdiv(wh[0] * RATE_PEAK[kind], 1000) + tdiv(wh[1] * RATE_SHOULDER[kind], 1000)
              + tdiv(wh[2] * RATE_OFF[kind], 1000))
    credit = tdiv(export * EXPORT_CREDIT[kind], 1000)
    peak_w = peak_wh * 2
    demand = tdiv(peak_w * DEMAND[kind], 1000)
    supply = DAYS * SUPPLY[kind]
    pre = energy - credit + demand + supply
    total = pre + tdiv(pre * GST, 100)
    best_alt = -1
    saving = 0
    if kind == 0:
        imported = wh[0] + wh[1] + wh[2]
        flat = tdiv(imported * FLAT_RATE, 1000) - credit + supply
        dem = (tdiv(imported * DEMAND_TARIFF_ENERGY, 1000) - credit + supply
               + tdiv(window_peak * 2 * DEMAND_TARIFF_KW, 1000))
        for i, alt in enumerate([flat, dem]):
            alt_total = alt + tdiv(alt * GST, 100)
            if total - alt_total > saving:
                saving = total - alt_total
                best_alt = i
    return Bill(m.mid, m.kind, wh, export, total, best_alt, saving, peak_w)


def bill_all(meters):
    return [bill_one(m) for m, _ in meters]


# ------------------------------------------------------------ profiles

class Profile:
    def __init__(self, load, peak_k, low_k, by_kind_at_peak):
        self.load = load
        self.peak_k = peak_k
        self.low_k = low_k
        self.by_kind_at_peak = by_kind_at_peak


def profile(meters):
    load = [0] * WEEK
    for m, _ in meters:
        for k in range(WEEK):
            load[k] += m.reads[k]
    peak_k = 0
    low_k = 0
    for k in range(WEEK):
        if load[k] > load[peak_k]:
            peak_k = k
        if load[k] < load[low_k]:
            low_k = k
    by_kind = [0, 0, 0]
    for m, _ in meters:
        by_kind[KINDS.index(m.kind)] += m.reads[peak_k]
    return Profile(load, peak_k, low_k, by_kind)


# ------------------------------------------------------------ reports

def usage_report(rg, meters):
    counts = [0, 0, 0]
    solar = 0
    imported = 0
    exported = 0
    est = 0
    for m, filled in meters:
        counts[KINDS.index(m.kind)] += 1
        solar += m.solar
        est += filled
        for x in m.reads:
            if x < 0:
                exported -= x
            else:
                imported += x
    return "%s usage: %d meters (res %d, com %d, ind %d; %d with solar), %s kWh in, %s kWh out, %d reads estimated" % (
        rg.name, len(meters), counts[0], counts[1], counts[2], solar, kwh(imported), kwh(exported), est)


def bill_report(rg, bills):
    total = 0
    sums = [0, 0, 0]
    counts = [0, 0, 0]
    for b in bills:
        total += b.total
        k = KINDS.index(b.kind)
        sums[k] += b.total
        counts[k] += 1
    avg = []
    for k in range(3):
        avg.append("%s %s" % (KINDS[k], money(tdiv(sums[k], counts[k])) if counts[k] else "-"))
    top = sorted(range(len(bills)), key=lambda i: (-bills[i].total, bills[i].mid))[:3]
    tops = ", ".join("%s %s" % (bills[i].mid, money(bills[i].total)) for i in top)
    return "%s bills: %s in all; average %s; largest %s" % (rg.name, money(total), ", ".join(avg), tops)


def band_report(rg, bills):
    wh = [0, 0, 0]
    export = 0
    for b in bills:
        for i in range(3):
            wh[i] += b.wh[i]
        export += b.export
    all_wh = wh[0] + wh[1] + wh[2]
    return "%s bands: peak %s, shoulder %s, off-peak %s of %s kWh; exports %s of imports" % (
        rg.name, pct(wh[0], all_wh), pct(wh[1], all_wh), pct(wh[2], all_wh), kwh(all_wh), pct(export, all_wh))


def peak_report(rg, prof):
    peak = prof.load[prof.peak_k]
    total = sum(prof.load)
    avg = tdiv(total, WEEK)
    parts = ", ".join("%s %s" % (KINDS[k], pct(prof.by_kind_at_peak[k], peak)) for k in range(3))
    return "%s peak: %s kW at %s, low %s kW at %s, load factor %s; at the peak %s" % (
        rg.name, group3(tdiv(peak * 2, 1000)), when(prof.peak_k), group3(tdiv(prof.load[prof.low_k] * 2, 1000)),
        when(prof.low_k), pct(avg, peak), parts)


def tariff_report(rg, bills):
    n = [0, 0]
    save = [0, 0]
    res = 0
    for b in bills:
        if b.kind != "res":
            continue
        res += 1
        if b.best_alt >= 0:
            n[b.best_alt] += 1
            save[b.best_alt] += b.alt_saving
    return "%s tariffs: of %d residential meters, %d would pay less on flat (%s in all) and %d on the demand tariff (%s)" % (
        rg.name, res, n[0], money(save[0]), n[1], money(save[1]))


# ------------------------------------------------------------ settlement

def settle(mkt, rg, prof, bills):
    # Buy the region's import in every half hour from what the regions
    # settled before it left, cheapest tranche first; anything beyond is
    # bought at the scarcity price, and each run of short half hours
    # raises one numbered ticket.
    bought = [0] * (len(TRANCHES) + 1)
    cost = 0
    tickets = []
    short_run = False
    for k in range(WEEK):
        need = prof.load[k]
        if need <= 0:
            short_run = False
            continue
        left = mkt.left[k]
        for t in range(len(TRANCHES)):
            take = min(need, left[t])
            left[t] -= take
            need -= take
            bought[t] += take
            cost += tdiv(take * TRANCHE_PRICE[t], 1000)
        if need > 0:
            bought[len(TRANCHES)] += need
            cost += tdiv(need * SCARCITY, 1000)
            if not short_run:
                mkt.tickets += 1
                tid = "SF-%04d" % mkt.tickets
                tickets.append(tid)
                mkt.log += "%s %s %s short %s kWh\n" % (tid, rg.name, when(k), kwh(need))
            short_run = True
        else:
            short_run = False
        if k % SLOTS == SLOTS - 1:
            d = k // SLOTS
            mkt.log += "%s %s bought %s\n" % (rg.name, DAY_NAMES[d], ", ".join(
                "%s %s" % (name, kwh(x)) for name, x in zip(TRANCHES + ["scarcity"], bought)))
    energy_rev = 0
    for b in bills:
        kind = KINDS.index(b.kind)
        energy_rev += (tdiv(b.wh[0] * RATE_PEAK[kind], 1000) + tdiv(b.wh[1] * RATE_SHOULDER[kind], 1000)
                       + tdiv(b.wh[2] * RATE_OFF[kind], 1000))
    all_wh = sum(bought)
    avg = tdiv(cost * 10000, all_wh) if all_wh else 0     # cents per MWh
    first = (" from " + tickets[0]) if tickets else ""
    plural = "" if len(tickets) == 1 else "s"
    return "%s settled: %s kWh (%s), %s at $%d.%02d/MWh, %d shortfall ticket%s%s; margin on energy %s" % (
        rg.name, kwh(all_wh), ", ".join("%s %s" % (TRANCHES[t], pct(bought[t], all_wh)) for t in range(len(TRANCHES))),
        money(cost), tdiv(avg, 100), avg % 100, len(tickets), plural, first, money(energy_rev - cost))


# ------------------------------------------------------------ the job

def run():
    north = Region("north", "NR", 11, [72, 20, 8], 30)
    coast = Region("coast", "CO", 23, [64, 26, 10], 45)
    valley = Region("valley", "VA", 37, [55, 15, 30], 20)
    metro = Region("metro", "ME", 53, [40, 48, 12], 15)

    m1 = build_meters(north)
    m2 = build_meters(coast)
    m3 = build_meters(valley)
    m4 = build_meters(metro)

    write_text("north.csv", encode_meters(m1))
    write_text("coast.csv", encode_meters(m2))
    write_text("valley.csv", encode_meters(m3))
    write_text("metro.csv", encode_meters(m4))

    whole = read_text("coast.csv")
    write_text("coast.bad", damage(whole, 300, 30, "4.5"))

    t1 = read_text("north.csv")
    t2 = read_text("coast.csv")
    t3 = read_text("valley.csv")
    t4 = read_text("metro.csv")
    for name, t in [("north", t1), ("coast", t2), ("valley", t3), ("metro", t4)]:
        print("%s.csv: %s bytes, fnv %08x" % (name, group3(len(t.encode())), text_sum(t)))

    g1 = parse_meters("north.csv", north, t1)
    g2 = parse_meters("coast.csv", coast, t2)
    g3 = parse_meters("valley.csv", valley, t3)
    g4 = parse_meters("metro.csv", metro, t4)
    try:
        parse_meters("coast.bad", coast, read_text("coast.bad"))
        print("coast.bad: parsed")
    except BadData as e:
        print(e)
    try:
        parse_meters("metro.csv", north, t4)
        print("metro.csv as north: parsed")
    except BadData as e:
        print(e)
    same = same_meters(g1, m1) and same_meters(g2, m2) and same_meters(g3, m3) and same_meters(g4, m4)
    print("parsed meters match the built ones: %s" % ("true" if same else "false"))

    e1 = estimate(g1)
    e2 = estimate(g2)
    e3 = estimate(g3)
    e4 = estimate(g4)

    b1 = bill_all(e1)
    b2 = bill_all(e2)
    b3 = bill_all(e3)
    b4 = bill_all(e4)

    p1 = profile(e1)
    p2 = profile(e2)
    p3 = profile(e3)
    p4 = profile(e4)

    reports = [
        usage_report(north, e1), usage_report(coast, e2), usage_report(valley, e3), usage_report(metro, e4),
        bill_report(north, b1), bill_report(coast, b2), bill_report(valley, b3), bill_report(metro, b4),
        band_report(north, b1), band_report(coast, b2), band_report(valley, b3), band_report(metro, b4),
        peak_report(north, p1), peak_report(coast, p2), peak_report(valley, p3), peak_report(metro, p4),
        tariff_report(north, b1), tariff_report(coast, b2), tariff_report(valley, b3), tariff_report(metro, b4),
    ]
    for r in reports:
        print(r)

    mkt = Market()
    print(settle(mkt, north, p1, b1))
    print(settle(mkt, coast, p2, b2))
    print(settle(mkt, valley, p3, b3))
    print(settle(mkt, metro, p4, b4))
    write_text("settlement.log", mkt.log)
    back = read_text("settlement.log")
    rows = len(back.split("\n")) - 1
    print("settlement.log: %d entries, %s bytes, fnv %08x" % (rows, group3(len(back.encode())), text_sum(back)))

    for p in ["north.csv", "coast.csv", "valley.csv", "metro.csv", "coast.bad", "settlement.log"]:
        os.remove(p)


try:
    run()
except BadData as e:
    print("meter_settle failed: %s" % e)
