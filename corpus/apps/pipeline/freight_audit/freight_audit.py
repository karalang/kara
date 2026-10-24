"""pipeline/freight_audit, the Python mirror: a shipper's October 2026
freight-payment batch over four carriers (Parcelo parcel, Swift expedited
air, Northline less-than-truckload and Coastal truckload), from the
bill-of-lading file and each carrier's invoice file to the audit that
re-rates every invoice against the contract tariffs, the per-carrier
reports, and one payment schedule whose weekly cash budget every
carrier's approved invoices draw on. It wrote expected.out.

Run from this directory: python3 -I -B freight_audit.py > expected.out
FREIGHT_REVERSE=1 schedules the carriers in reverse order."""
import os

MASK32 = 4294967295
# Day 0 is Thursday 1 October 2026.
FIRST_WEEKDAY = 3
WEEKDAYS = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"]
MONTH_START = [0, 31, 59, 90, 120, 151, 181, 212, 243, 273, 304, 334]
MONTH_DAYS = [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
DAY0 = 273
MONTH_LEN = 31
SHIPMENTS = 2000
CONSIGNEES = 60
REGIONS = 40
# The shipper's three distribution centres: name, ZIP3 and map position
# in miles.
DC_NAMES = ["Newark", "Columbus", "Reno"]
DC_ZIPS = ["071", "432", "895"]
DC_X = [2350, 1640, 260]
DC_Y = [620, 700, 760]
# The LTL freight classes and the accessorial codes: liftgate,
# residential, inside delivery.
CLASSES = [50, 55, 60, 65, 70, 77, 85, 92, 100, 110, 125, 150, 175, 200, 250, 300, 400, 500]
ACC_CODES = "LRI"
# LTL weight breaks (pounds) and what each pays of the base, per mille.
BREAKS = [500, 1000, 2000, 5000]
BREAK_FACTOR = [1000, 880, 760, 650, 550]
LTL_ACC = [7500, 9500, 12000]
VERDICTS = ["OK", "OVER", "UNDER", "WGT", "ACC", "DUP", "UNK", "GSR"]
VERDICT_NAMES = ["billed as rated", "over-billed", "under-billed", "billed weight is not the BOL's",
                 "accessorial not authorized", "duplicate invoice", "no such shipment for this carrier",
                 "late, refunded under the service guarantee"]
TOLERANCE = 100
# Payment runs every Friday from 9 October, each with the same cash budget.
RUN_DAYS = [8, 15, 22, 29, 36, 43, 50, 57]
RUN_BUDGET = 20000000
APPROVAL_DAYS = 3
DISCOUNT_DAYS = 10
NET_DAYS = 30

SHIP_HEADER = "bol,day,dc,consignee,zip,residential,carrier,weight,length,width,height,class,stops,wait,acc,promised,delivered"
INV_HEADER = "invoice,day,bol,weight,acc,amount"


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


def tenths(num, den):
    # num / den with one decimal, rounded half up
    t = (num * 20 + den) // (2 * den)
    return f"{t // 10}.{t % 10}"


def is_digits(t):
    return len(t) > 0 and all("0" <= ch <= "9" for ch in t)


def cents_of(t):
    dot = t.find(".")
    if dot < 1 or len(t) - dot != 3:
        return None
    a, b = t[:dot], t[dot + 1:]
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


def date_of(day):
    n = DAY0 + day
    m = 11
    while MONTH_START[m] > n:
        m -= 1
    return f"2026-{m + 1:02}-{n - MONTH_START[m] + 1:02}"


def day_of(t):
    if len(t) != 10 or t[4] != "-" or t[7] != "-":
        return None
    y, m, d = t[0:4], t[5:7], t[8:10]
    if not (is_digits(y) and is_digits(m) and is_digits(d)):
        return None
    y, m, d = int(y), int(m), int(d)
    if y != 2026 or m < 1 or m > 12 or d < 1 or d > MONTH_DAYS[m - 1]:
        return None
    return MONTH_START[m - 1] + d - 1 - DAY0


def short_date(day):
    return date_of(day)[5:]


def isqrt(n):
    # Newton's method from above
    if n < 2:
        return n
    x = n
    y = (x + 1) // 2
    while y < x:
        x = y
        y = (x + n // x) // 2
    return x


def percent_of(c, pm):
    # pm per ten thousand of c, rounded half up
    return (c * pm + 5000) // 10000


# ------------------------------------------------------------ the world

class Carrier:
    def __init__(self, name, code, kind, seed, strangers):
        self.name = name
        self.code = code
        # 0 parcel, 1 air, 2 LTL, 3 truckload
        self.kind = kind
        self.seed = seed
        self.strangers = strangers


class Shipment:
    def __init__(self, bol, day, dc, consignee, zip3, residential, carrier, weight, length, width, height,
                 cls, stops, wait, acc, promised, delivered):
        self.bol = bol
        self.day = day
        self.dc = dc
        self.consignee = consignee
        self.zip3 = zip3
        self.residential = residential
        self.carrier = carrier
        self.weight = weight
        self.length = length
        self.width = width
        self.height = height
        self.cls = cls
        self.stops = stops
        self.wait = wait
        self.acc = acc
        self.promised = promised
        self.delivered = delivered

    def key(self):
        return (self.bol, self.day, self.dc, self.consignee, self.zip3, self.residential, self.carrier,
                self.weight, self.length, self.width, self.height, self.cls, self.stops, self.wait, self.acc,
                self.promised, self.delivered)


class Invoice:
    def __init__(self, inv, day, bol, weight, acc, amount):
        self.inv = inv
        self.day = day
        self.bol = bol
        self.weight = weight
        self.acc = acc
        self.amount = amount

    def key(self):
        return (self.inv, self.day, self.bol, self.weight, self.acc, self.amount)


class Outcome:
    def __init__(self, verdict, audited, approved, ship):
        # an index into VERDICTS
        self.verdict = verdict
        self.audited = audited
        self.approved = approved
        # the shipment's index, or -1
        self.ship = ship


def region_zip(i):
    return f"{100 + (i * 37) % 899:03}"


def region_x(i):
    return mix(7, i, 1) % 2600


def region_y(i):
    return mix(7, i, 2) % 1400


def consignee_region(c):
    return mix(9, c, 1) % REGIONS


def consignee_residential(c):
    return mix(9, c, 2) % 5 == 0


def miles(dc, zip3):
    r = ZIP_REGION[zip3]
    dx = DC_X[dc] - region_x(r)
    dy = DC_Y[dc] - region_y(r)
    m = isqrt(dx * dx + dy * dy) * 117 // 100
    return max(m, 25)


ZIP_REGION = {region_zip(i): i for i in range(REGIONS)}


def zone_of(m):
    return min(2 + m // 300, 8)


def carrier_of(code):
    return "PSNC".index(code)


def build_shipments():
    out = []
    for k in range(SHIPMENTS):
        day = k * MONTH_LEN // SHIPMENTS
        dc = mix(11, k, 0) % 3
        c = mix(11, k, 1) % CONSIGNEES
        zip3 = region_zip(consignee_region(c))
        res = consignee_residential(c)
        r = mix(11, k, 2) % 100
        length = width = height = cls = stops = wait = 0
        acc = ""
        m = miles(dc, zip3)
        if r < 46:
            carrier = "P"
            weight = 1 + mix(11, k, 3) % 70
        elif r < 54:
            carrier = "S"
            weight = 1 + mix(11, k, 3) % 90
        elif r < 91:
            carrier = "N"
            weight = 150 + mix(11, k, 3) % 7851
        else:
            carrier = "C"
            weight = 9000 + mix(11, k, 3) % 33001
        if carrier in "PS":
            length = 6 + mix(11, k, 4) % 31
            width = 4 + mix(11, k, 5) % 21
            height = 2 + mix(11, k, 6) % 17
            if res:
                acc = "R"
            promised = 1 if carrier == "S" else 1 + m // 600
        elif carrier == "N":
            cls = CLASSES[mix(11, k, 4) % len(CLASSES)]
            if mix(11, k, 5) % 6 == 0:
                acc += "L"
            if res:
                acc += "R"
            if mix(11, k, 6) % 10 == 0:
                acc += "I"
            promised = 1 + m // 450
        else:
            stops = 1 + mix(11, k, 4) % 3
            wait = mix(11, k, 5) % 7
            promised = 1 + m // 500
        late = mix(11, k, 7) % 90
        delay = 2 if late < 3 else 1 if late < 13 else 0
        out.append(Shipment(f"B{k + 1:05}", day, dc, f"C{c + 1:04}", zip3, res, carrier, weight, length, width,
                            height, cls, stops, wait, acc, promised, day + promised + delay))
    return out


def ceil_div(a, b):
    return (a + b - 1) // b


# The contract tariff: what a shipment should cost at a given weight with
# the given accessorials.
def rate(kind, s, weight, acc):
    m = miles(s.dc, s.zip3)
    zone = zone_of(m)
    if kind == 0:
        dim = ceil_div(s.length * s.width * s.height, 139)
        billable = max(weight, dim)
        sub = 900 + 110 * (zone - 2) + billable * (35 + 9 * (zone - 2))
        if "R" in acc:
            sub += 460
        return sub + percent_of(sub, 1425)
    if kind == 1:
        dim = ceil_div(s.length * s.width * s.height, 166)
        billable = max(weight, dim)
        sub = max(2500 + billable * (110 + 22 * (zone - 2)), 4500)
        if "R" in acc:
            sub += 600
        return sub + percent_of(sub, 1800)
    if kind == 2:
        cwt = ceil_div(weight, 100)
        base_cwt = (1800 + m * 4) * s.cls // 100
        b = 0
        while b < len(BREAKS) and weight >= BREAKS[b]:
            b += 1
        gross = cwt * base_cwt * BREAK_FACTOR[b] // 1000
        net = max(gross * 32 // 100, 9500)
        for i in range(3):
            if ACC_CODES[i] in acc:
                net += LTL_ACC[i]
        return net + percent_of(net, 2240)
    linehaul = max(m * 285, 85000)
    extra = 15000 * (s.stops - 1) + 7500 * max(s.wait - 2, 0)
    if "L" in acc:
        extra += 9000
    return linehaul + extra + m * 62


def acc_text(acc):
    return acc if acc != "" else "-"


def build_invoices(cr, ships):
    raw = []
    order = 0
    mine = [s for s in ships if s.carrier == cr.code]
    for k, s in enumerate(mine):
        e = mix(cr.seed, k, 5) % 1000
        day = s.delivered + 1 + mix(cr.seed, k, 6) % 3
        weight = s.weight
        acc = s.acc
        if e < 40:
            weight = s.weight * (110 + mix(cr.seed, k, 7) % 30) // 100
            amount = rate(cr.kind, s, weight, acc)
        elif e < 90:
            extra = [ch for ch in ("R" if cr.kind < 2 else "LI" if cr.kind == 2 else "L") if ch not in acc]
            if extra:
                acc = "".join(ch for ch in ACC_CODES if ch in acc or ch == extra[0])
            amount = rate(cr.kind, s, weight, acc)
        elif e < 125:
            amount = rate(cr.kind, s, weight, acc) * (103 + mix(cr.seed, k, 7) % 10) // 100
        elif e < 140:
            amount = rate(cr.kind, s, weight, acc) * (90 + mix(cr.seed, k, 7) % 8) // 100
        elif e < 155:
            amount = rate(cr.kind, s, weight, acc) + mix(cr.seed, k, 7) % 81
        else:
            amount = rate(cr.kind, s, weight, acc)
        raw.append((day, order, s.bol, weight, acc, amount))
        order += 1
        if mix(cr.seed, k, 8) % 40 == 0:
            raw.append((day + 4 + mix(cr.seed, k, 9) % 6, order, s.bol, weight, acc, amount))
            order += 1
    # bills for shipments that are not this carrier's: some on no BOL at
    # all, some on another carrier's
    for j in range(cr.strangers):
        h = mix(cr.seed, j, 10)
        if j % 3 == 2:
            other = [s for s in ships if s.carrier != cr.code]
            s = other[h % len(other)]
            raw.append((s.delivered + 2, order, s.bol, s.weight, "", 5000 + mix(cr.seed, j, 11) % 85000))
        else:
            raw.append((3 + h % 36, order, f"B{90000 + h % 9000:05}", 1 + mix(cr.seed, j, 12) % 900, "",
                        5000 + mix(cr.seed, j, 11) % 85000))
        order += 1
    raw.sort()
    out = []
    for n, (day, _, bol, weight, acc, amount) in enumerate(raw):
        out.append(Invoice(f"{cr.code}{n + 1:06}", day, bol, weight, acc, amount))
    return out


def encode_shipments(ships):
    lines = [SHIP_HEADER]
    for s in ships:
        lines.append(",".join([s.bol, date_of(s.day), DC_ZIPS[s.dc], s.consignee, s.zip3, "Y" if s.residential else "N",
                               s.carrier, str(s.weight), str(s.length), str(s.width), str(s.height), str(s.cls),
                               str(s.stops), str(s.wait), acc_text(s.acc), str(s.promised), date_of(s.delivered)]))
    return "\n".join(lines) + "\n"


def encode_invoices(invs):
    lines = [INV_HEADER]
    for v in invs:
        lines.append(",".join([v.inv, date_of(v.day), v.bol, str(v.weight), acc_text(v.acc), dollars(v.amount)]))
    return "\n".join(lines) + "\n"


def read_text(path):
    try:
        with open(path, encoding="utf-8", newline="") as f:
            return f.read()
    except OSError:
        raise BatchError(f"cannot read {path}")


def write_text(path, text):
    with open(path, "w", encoding="utf-8", newline="") as f:
        f.write(text)


# Replace a field of line n (counting the header as line 0).
def damage(text, n, field, value):
    lines = text.split("\n")
    f = lines[n].split(",")
    f[field] = value
    lines[n] = ",".join(f)
    return "\n".join(lines)


# ------------------------------------------------------------ parsing

def body_lines(path, text, header):
    lines = text.split("\n")
    if len(lines) < 2 or lines[-1] != "":
        raise BatchError(f"{path}: no final newline")
    if lines[0] != header:
        raise BatchError(f"{path} line 1: bad header")
    return lines[1:-1]


def int_field(path, n, what, t, lo, hi):
    v = small_int(t, lo, hi)
    if v is None:
        raise BatchError(f"{path} line {n}: bad {what} '{t}'")
    return v


def day_field(path, n, what, t, lo, hi):
    v = day_of(t)
    if v is None or v < lo or v > hi:
        raise BatchError(f"{path} line {n}: bad {what} '{t}'")
    return v


def acc_field(path, n, t, allowed):
    if t == "-":
        return ""
    k = 0
    for ch in t:
        if ch not in allowed or ACC_CODES.index(ch) < k:
            raise BatchError(f"{path} line {n}: bad accessorials '{t}'")
        k = ACC_CODES.index(ch) + 1
    return t


def bol_ok(t):
    return len(t) == 6 and t[0] == "B" and is_digits(t[1:])


def parse_shipments(path, text):
    body = body_lines(path, text, SHIP_HEADER)
    out = []
    seen = set()
    last = 0
    for k, line in enumerate(body):
        n = k + 2
        f = line.split(",")
        if len(f) != 17:
            raise BatchError(f"{path} line {n}: {len(f)} fields, not 17")
        if not bol_ok(f[0]):
            raise BatchError(f"{path} line {n}: bad BOL '{f[0]}'")
        if f[0] in seen:
            raise BatchError(f"{path} line {n}: BOL {f[0]} twice")
        seen.add(f[0])
        day = day_field(path, n, "ship date", f[1], last, MONTH_LEN - 1)
        last = day
        if f[2] not in DC_ZIPS:
            raise BatchError(f"{path} line {n}: no DC at '{f[2]}'")
        dc = DC_ZIPS.index(f[2])
        if len(f[3]) != 5 or f[3][0] != "C" or not is_digits(f[3][1:]):
            raise BatchError(f"{path} line {n}: bad consignee '{f[3]}'")
        if f[4] not in ZIP_REGION:
            raise BatchError(f"{path} line {n}: no lane to '{f[4]}'")
        if f[5] not in ("Y", "N"):
            raise BatchError(f"{path} line {n}: bad residential flag '{f[5]}'")
        if len(f[6]) != 1 or f[6] not in "PSNC":
            raise BatchError(f"{path} line {n}: bad carrier '{f[6]}'")
        kind = carrier_of(f[6])
        weight = int_field(path, n, "weight", f[7], 1, 45000)
        small = kind < 2
        length = int_field(path, n, "length", f[8], 1 if small else 0, 108 if small else 0)
        width = int_field(path, n, "width", f[9], 1 if small else 0, 108 if small else 0)
        height = int_field(path, n, "height", f[10], 1 if small else 0, 108 if small else 0)
        cls = int_field(path, n, "class", f[11], 0, 500)
        if (kind == 2) != (cls in CLASSES):
            raise BatchError(f"{path} line {n}: bad class '{f[11]}'")
        stops = int_field(path, n, "stops", f[12], 1 if kind == 3 else 0, 9 if kind == 3 else 0)
        wait = int_field(path, n, "wait", f[13], 0, 24 if kind == 3 else 0)
        acc = acc_field(path, n, f[14], ["R", "R", "LRI", "L"][kind])
        promised = int_field(path, n, "promised days", f[15], 1, 9)
        delivered = day_field(path, n, "delivery date", f[16], day, day + 20)
        out.append(Shipment(f[0], day, dc, f[3], f[4], f[5] == "Y", f[6], weight, length, width, height, cls, stops,
                            wait, acc, promised, delivered))
    return out


def parse_invoices(path, cr, text):
    body = body_lines(path, text, INV_HEADER)
    out = []
    last = 0
    for k, line in enumerate(body):
        n = k + 2
        f = line.split(",")
        if len(f) != 6:
            raise BatchError(f"{path} line {n}: {len(f)} fields, not 6")
        if len(f[0]) != 7 or f[0][0] != cr.code or not is_digits(f[0][1:]):
            raise BatchError(f"{path} line {n}: invoice '{f[0]}' is not {cr.name}'s")
        day = day_of(f[1])
        if day is None or day < 0 or day > 60:
            raise BatchError(f"{path} line {n}: bad invoice date '{f[1]}'")
        if day < last:
            raise BatchError(f"{path} line {n}: invoice date {f[1]} is before the line above")
        last = day
        if not bol_ok(f[2]):
            raise BatchError(f"{path} line {n}: bad BOL '{f[2]}'")
        weight = int_field(path, n, "weight", f[3], 1, 60000)
        acc = acc_field(path, n, f[4], ACC_CODES)
        amount = cents_of(f[5])
        if amount is None:
            raise BatchError(f"{path} line {n}: bad amount '{f[5]}'")
        out.append(Invoice(f[0], day, f[2], weight, acc, amount))
    return out


# ------------------------------------------------------------ the audit

def audit(cr, ships, invs):
    by_bol = {s.bol: i for i, s in enumerate(ships)}
    seen = set()
    out = []
    for v in invs:
        i = by_bol.get(v.bol, -1)
        if i < 0 or ships[i].carrier != cr.code:
            out.append(Outcome(6, 0, 0, -1))
            continue
        s = ships[i]
        audited = rate(cr.kind, s, s.weight, s.acc)
        if v.bol in seen:
            out.append(Outcome(5, audited, 0, i))
            continue
        seen.add(v.bol)
        if cr.kind == 1 and s.delivered > s.day + s.promised:
            out.append(Outcome(7, audited, 0, i))
        elif v.weight != s.weight:
            out.append(Outcome(3, audited, audited, i))
        elif any(ch not in s.acc for ch in v.acc):
            out.append(Outcome(4, audited, audited, i))
        elif v.amount > audited + TOLERANCE:
            out.append(Outcome(1, audited, audited, i))
        elif v.amount < audited - TOLERANCE:
            out.append(Outcome(2, audited, v.amount, i))
        else:
            out.append(Outcome(0, audited, v.amount, i))
    return out


# ------------------------------------------------------------ reports

def audit_report(cr, invs, outs):
    counts = [0] * len(VERDICTS)
    first = [-1] * len(VERDICTS)
    billed = approved = 0
    for k, (v, o) in enumerate(zip(invs, outs)):
        counts[o.verdict] += 1
        if first[o.verdict] < 0:
            first[o.verdict] = k
        billed += v.amount
        approved += o.approved
    tally = ", ".join(f"{VERDICTS[c]} {counts[c]}" for c in range(len(VERDICTS)) if counts[c] > 0)
    lines = [f"{cr.name}: {group3(len(invs))} invoices, {money(billed)} billed, {money(approved)} approved, saved "
             f"{money(billed - approved)} ({tenths((billed - approved) * 100, billed)}%): {tally}"]
    for c in range(1, len(VERDICTS)):
        if first[c] >= 0:
            v = invs[first[c]]
            o = outs[first[c]]
            what = f"billed {money(v.amount)}"
            if o.ship >= 0:
                what += f", audited {money(o.audited)}"
            lines.append(f"  {VERDICTS[c]} {VERDICT_NAMES[c]}: {v.inv} {v.bol} {what}")
    return lines


def spend_report(cr, ships, outs):
    by_dc = [0] * 3
    n_dc = [0] * 3
    lanes = {}
    weight = 0
    total = 0
    n = 0
    for o in outs:
        if o.approved == 0:
            continue
        s = ships[o.ship]
        by_dc[s.dc] += o.approved
        n_dc[s.dc] += 1
        lane = f"{DC_ZIPS[s.dc]}-{s.zip3}"
        got = lanes.get(lane, (0, 0))
        lanes[lane] = (got[0] + o.approved, got[1] + 1)
        weight += s.weight
        total += o.approved
        n += 1
    best = sorted(lanes.items(), key=lambda kv: (-kv[1][0], kv[0]))[0]
    dcs = ", ".join(f"{DC_NAMES[d]} {money(by_dc[d])} ({n_dc[d]})" for d in range(3))
    return (f"{cr.name} spend: {money(total)} on {group3(n)} shipments, {money(total * 100 // weight)} a hundredweight; "
            f"{dcs}; top lane {best[0]} {money(best[1][0])} on {best[1][1]}")


def service_report(cr, ships):
    mine = [s for s in ships if s.carrier == cr.code]
    on_time = 0
    late1 = 0
    late2 = 0
    transit = 0
    worst = None
    for s in mine:
        took = s.delivered - s.day
        transit += took
        over = took - s.promised
        if over <= 0:
            on_time += 1
        elif over == 1:
            late1 += 1
        else:
            late2 += 1
        if over > 0 and (worst is None or over > worst[0] or (over == worst[0] and s.day > worst[1].day)):
            worst = (over, s)
    text = (f"{cr.name} service: {group3(on_time)} of {group3(len(mine))} on time ({tenths(on_time * 100, len(mine))}%), "
            f"{tenths(transit, len(mine))} days in transit on average, {late1} a day late, {late2} two days late")
    if worst is not None:
        s = worst[1]
        text += f"; latest late {s.bol} shipped {short_date(s.day)}, promised {s.promised}, took {s.delivered - s.day}"
    return text


def consignee_report(cr, ships, outs):
    spend = {}
    residential = 0
    for o in outs:
        if o.approved == 0:
            continue
        s = ships[o.ship]
        spend[s.consignee] = spend.get(s.consignee, 0) + o.approved
        if s.residential:
            residential += 1
    top = sorted(spend.items(), key=lambda kv: (-kv[1], kv[0]))[:3]
    return (f"{cr.name} consignees: {len(spend)} billed, {residential} residential deliveries; top "
            + ", ".join(f"{c} {money(v)}" for c, v in top))


# ------------------------------------------------------------ the payment run

class Schedule:
    def __init__(self):
        self.left = [RUN_BUDGET] * len(RUN_DAYS)
        self.paid = 0
        self.discounts = 0
        self.fees = 0
        self.unpaid = 0
        self.unpaid_n = 0
        self.log = []


def due_on(run_day, inv_day, approved):
    if run_day <= inv_day + DISCOUNT_DAYS:
        return approved - (approved * 2 + 50) // 100
    if run_day > inv_day + NET_DAYS:
        return approved + (approved * 15 + 500) // 1000
    return approved


def schedule(pay, cr, invs, outs):
    n = 0
    paid = 0
    disc_n = 0
    disc = 0
    late_n = 0
    fees = 0
    unpaid_n = 0
    unpaid = 0
    runs_used = set()
    for v, o in zip(invs, outs):
        if o.approved == 0:
            continue
        ready = v.day + APPROVAL_DAYS
        placed = False
        for r in range(len(RUN_DAYS)):
            if RUN_DAYS[r] < ready:
                continue
            due = due_on(RUN_DAYS[r], v.day, o.approved)
            if due > pay.left[r]:
                continue
            pay.left[r] -= due
            runs_used.add(r)
            n += 1
            paid += due
            if due < o.approved:
                disc_n += 1
                disc += o.approved - due
            elif due > o.approved:
                late_n += 1
                fees += due - o.approved
            pay.log.append(f"{date_of(RUN_DAYS[r])} {v.inv} {v.bol} {dollars(o.approved)} paid {dollars(due)}")
            placed = True
            break
        if not placed:
            unpaid_n += 1
            unpaid += o.approved
            pay.log.append(f"unpaid {v.inv} {v.bol} {dollars(o.approved)}")
    pay.paid += paid
    pay.discounts += disc
    pay.fees += fees
    pay.unpaid += unpaid
    pay.unpaid_n += unpaid_n
    return (f"{cr.name} paid: {group3(n)} invoices, {money(paid)} over {len(runs_used)} runs; {disc_n} took the 2% "
            f"discount, saving {money(disc)}; {late_n} paid past net {NET_DAYS}, {money(fees)} in late fees; "
            f"{unpaid_n} unpaid at the cutoff, {money(unpaid)}")


def schedule_summary(pay):
    runs = ", ".join(f"{short_date(RUN_DAYS[r])} {money(RUN_BUDGET - pay.left[r])}" for r in range(len(RUN_DAYS)))
    return [f"payments: {money(pay.paid)} paid, {money(pay.discounts)} in discounts taken, {money(pay.fees)} in late "
            f"fees, {pay.unpaid_n} invoices unpaid at the cutoff ({money(pay.unpaid)})",
            f"runs of {money(RUN_BUDGET)}: {runs}"]


def file_line(name, text):
    rows = len(text.split("\n")) - 2
    return f"{name}: {group3(rows)} rows, {group3(len(text))} bytes, fnv {text_sum(text):08x}"


# ------------------------------------------------------------ the job

def run():
    parcelo = Carrier("parcelo", "P", 0, 501, 9)
    swift = Carrier("swift", "S", 1, 602, 6)
    northline = Carrier("northline", "N", 2, 703, 9)
    coastal = Carrier("coastal", "C", 3, 804, 6)

    ships = build_shipments()
    x1 = build_invoices(parcelo, ships)
    x2 = build_invoices(swift, ships)
    x3 = build_invoices(northline, ships)
    x4 = build_invoices(coastal, ships)

    write_text("shipments.csv", encode_shipments(ships))
    write_text("parcelo_invoices.csv", encode_invoices(x1))
    write_text("swift_invoices.csv", encode_invoices(x2))
    write_text("northline_invoices.csv", encode_invoices(x3))
    write_text("coastal_invoices.csv", encode_invoices(x4))

    # A copy of Northline's file with one amount cut short, and a copy of
    # Parcelo's with one invoice dated before the line above it.
    whole = read_text("northline_invoices.csv")
    amount = whole.split("\n")[57].split(",")[5]
    write_text("northline_invoices.bad", damage(whole, 57, 5, amount[:-1]))
    whole = read_text("parcelo_invoices.csv")
    earlier = whole.split("\n")[40].split(",")[1]
    write_text("parcelo_invoices.bad", damage(whole, 300, 1, earlier))

    ts = read_text("shipments.csv")
    t1 = read_text("parcelo_invoices.csv")
    t2 = read_text("swift_invoices.csv")
    t3 = read_text("northline_invoices.csv")
    t4 = read_text("coastal_invoices.csv")
    print(file_line("shipments", ts))
    print(file_line("parcelo", t1))
    print(file_line("swift", t2))
    print(file_line("northline", t3))
    print(file_line("coastal", t4))

    gs = parse_shipments("shipments.csv", ts)
    g1 = parse_invoices("parcelo_invoices.csv", parcelo, t1)
    g2 = parse_invoices("swift_invoices.csv", swift, t2)
    g3 = parse_invoices("northline_invoices.csv", northline, t3)
    g4 = parse_invoices("coastal_invoices.csv", coastal, t4)
    for path, cr in [("northline_invoices.bad", northline), ("parcelo_invoices.bad", parcelo)]:
        try:
            parse_invoices(path, cr, read_text(path))
            print(f"{path}: parsed")
        except BatchError as e:
            print(e)
    try:
        parse_invoices("coastal_invoices.csv", swift, t4)
        print("coastal_invoices.csv: parsed")
    except BatchError as e:
        print(e)
    same = ([s.key() for s in gs] == [s.key() for s in ships]
            and all([v.key() for v in g] == [v.key() for v in b] for g, b in [(g1, x1), (g2, x2), (g3, x3), (g4, x4)]))
    print(f"parsed files match the built ones: {'true' if same else 'false'}")

    o1 = audit(parcelo, gs, g1)
    o2 = audit(swift, gs, g2)
    o3 = audit(northline, gs, g3)
    o4 = audit(coastal, gs, g4)

    for cr, g, o in [(parcelo, g1, o1), (swift, g2, o2), (northline, g3, o3), (coastal, g4, o4)]:
        for line in audit_report(cr, g, o):
            print(line)
    for cr, o in [(parcelo, o1), (swift, o2), (northline, o3), (coastal, o4)]:
        print(spend_report(cr, gs, o))
    for cr in [parcelo, swift, northline, coastal]:
        print(service_report(cr, gs))
    for cr, o in [(parcelo, o1), (swift, o2), (northline, o3), (coastal, o4)]:
        print(consignee_report(cr, gs, o))

    pay = Schedule()
    order = [(parcelo, g1, o1), (swift, g2, o2), (northline, g3, o3), (coastal, g4, o4)]
    if os.environ.get("FREIGHT_REVERSE") == "1":
        order.reverse()
    for cr, g, o in order:
        print(schedule(pay, cr, g, o))
    for line in schedule_summary(pay):
        print(line)
    write_text("payments.log", "\n".join(pay.log) + "\n")
    back = read_text("payments.log")
    rows = len(back.split("\n")) - 1
    print(f"payments.log: {group3(rows)} entries, {group3(len(back))} bytes, fnv {text_sum(back):08x}")

    for p in ["shipments.csv", "parcelo_invoices.csv", "swift_invoices.csv", "northline_invoices.csv",
              "coastal_invoices.csv", "northline_invoices.bad", "parcelo_invoices.bad", "payments.log"]:
        os.remove(p)


def main():
    try:
        run()
    except BatchError as e:
        print(f"freight_audit failed: {e}")


main()
