"""pipeline/hotel_audit, the Python mirror: a hotel group's week-end night
audit over four hotels, from the reservation and folio-posting files to
the room-night grid that walks oversold arrivals, rate and tax posting,
no-show and late-cancellation fees, folios, reports, and one shared
loyalty ledger that posts every checked-out stay and moves members up
the tiers. It wrote expected.out.

Run from this directory: python3 -I -B hotel_audit.py > expected.out"""
import os

MASK32 = 4294967295
NIGHTS = 7
# Night 0 is Saturday 17 October 2026; a day is an index from it.
FIRST_WEEKDAY = 5
WEEKDAYS = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"]
ROOM_TYPES = ["K", "Q", "S"]
RATE_CODES = ["BAR", "AAA", "CORP", "GRP", "OTA"]
# Per rate code: the share of the rack rate in percent, whether Friday
# and Saturday nights carry the weekend uplift, and the segment.
RATE_PCT = [100, 90, 85, 78, 100]
RATE_WEEKEND = [True, True, False, False, True]
SEGMENTS = ["transient", "transient", "corporate", "group", "ota"]
SEGMENT_NAMES = ["transient", "corporate", "group", "ota"]
WEEKEND_PCT = 15
OTA_COMMISSION_BP = 1800
WALK_COST = 12000
FREE_CANCEL_DAYS = 2
DEPTS = ["RS", "MB", "PK", "SP", "RT", "PY"]
DEPT_NAMES = ["room service", "minibar", "parking", "spa", "restaurant"]
SURNAMES = ["Abara", "Brandt", "Castillo", "Dubois", "Eriksen", "Fontaine", "Garcia", "Haddad", "Ivanova", "Jensen",
            "Kowalski", "Laurent", "Moreau", "Nakamura", "Okafor", "Petrov", "Quinn", "Rossi", "Sato", "Tanaka",
            "Ueda", "Varga", "Weber", "Xu", "Yilmaz", "Zhang"]
MEMBERS = 400
# Loyalty tiers: the lifetime points that reach each, and the multiplier
# in percent on a stay's base points.
TIERS = ["member", "silver", "gold", "platinum"]
TIER_AT = [0, 20000, 45000, 90000]
TIER_PCT = [100, 125, 150, 200]
GOLD_BONUS = 2500
# Days from 1 January to the first of each month in 2026.
MONTH_START = [0, 31, 59, 90, 120, 151, 181, 212, 243, 273, 304, 334]
MONTH_DAYS = [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
DAY0 = MONTH_START[9] + 16


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


def pct1(num, den):
    # a percentage with one decimal, rounded half up
    if den == 0:
        return "0.0%"
    t = (num * 2000 + den) // (2 * den)
    return f"{t // 10}.{t % 10}%"


def is_digits(t):
    return len(t) > 0 and all("0" <= ch <= "9" for ch in t)


def cents_of(t):
    dot = t.find(".")
    if dot < 1 or len(t) - dot != 3:
        return None
    a, b = t[:dot], t[dot + 1:]
    if not (is_digits(a) and is_digits(b)) or len(a) > 6:
        return None
    return int(a) * 100 + int(b)


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


def weekday(day):
    return ((FIRST_WEEKDAY + day) % 7 + 7) % 7


def night_name(day):
    return f"{WEEKDAYS[weekday(day)]} {date_of(day)[5:]}"


class Hotel:
    def __init__(self, name, code, rooms, rack, seed, tax_bp, fee):
        self.name = name
        self.code = code
        self.rooms = rooms
        self.rack = rack
        self.seed = seed
        self.tax_bp = tax_bp
        self.fee = fee


class Res:
    def __init__(self, rid, guest, member, room, rate, arrival, nights, status, booked, cancelled):
        self.rid = rid
        self.guest = guest
        self.member = member
        self.room = room
        self.rate = rate
        self.arrival = arrival
        self.nights = nights
        self.status = status
        self.booked = booked
        self.cancelled = cancelled


class Post:
    def __init__(self, rid, day, dept, amount):
        self.rid = rid
        self.day = day
        self.dept = dept
        self.amount = amount


RES_KEYS = ["rid", "guest", "member", "room", "rate", "arrival", "nights", "status", "booked", "cancelled"]
POST_KEYS = ["rid", "day", "dept", "amount"]


# ------------------------------------------------------------ building

def build_reservations(ht):
    out = []
    total = sum(ht.rooms)
    count = total * 37 // 10
    for i in range(count):
        def r(salt):
            return mix(ht.seed, i, salt)
        rid = f"{ht.code}{100000 + i * 3:06}"
        guest = f"{SURNAMES[r(1) % len(SURNAMES)]} {chr(65 + r(2) % 26)}."
        member = "-"
        if r(3) % 5 < 2:
            member = f"M{1 + min(r(4) % MEMBERS, r(5) % MEMBERS):04}"
        tr = r(6) % total
        room = 0 if tr < ht.rooms[0] else (1 if tr < ht.rooms[0] + ht.rooms[1] else 2)
        cr = r(7) % 100
        rate = 0 if cr < 38 else (1 if cr < 46 else (2 if cr < 66 else (3 if cr < 78 else 4)))
        ar = r(8) % 24
        if ar < 2:
            arrival = -3 + ar
        elif ar < 3:
            arrival = -1
        elif ar < 8:
            arrival = 0
        elif ar < 12:
            arrival = 6
        else:
            arrival = 1 + r(9) % 5
        nr = r(10) % 20
        nights = 1 if nr < 6 else (2 if nr < 12 else (3 if nr < 16 else (4 if nr < 18 else 5 + r(11) % 3)))
        booked = arrival - 1 - r(12) % 70
        sr = r(13) % 100
        status = "B"
        cancelled = None
        if sr < 8:
            status = "X"
            cancelled = min(arrival, booked + 1 + r(14) % (arrival - booked))
        elif sr < 12:
            status = "N"
        out.append(Res(rid, guest, member, ROOM_TYPES[room], RATE_CODES[rate], arrival, nights, status, booked, cancelled))
    return out


def build_postings(ht, res):
    out = []
    for i, rv in enumerate(res):
        def r(salt):
            return mix(ht.seed, i, salt)
        if rv.status != "B":
            continue
        n = r(20) % 5
        for k in range(n):
            day = rv.arrival + r(21 + k) % rv.nights
            dept = r(26 + k) % 5
            amount = [1800, 600, 2500, 9000, 3500][dept] + r(31 + k) % [6000, 2400, 1500, 16000, 9000][dept]
            rid = rv.rid
            if r(36 + k) % 90 == 0:
                rid = f"{ht.code}{900000 + i:06}"
            out.append(Post(rid, day, DEPTS[dept], amount))
        if r(41) % 3 == 0:
            out.append(Post(rv.rid, rv.arrival, "PY", 10000 * (1 + r(42) % 4)))
    out.sort(key=lambda p: (p.day, p.rid, p.dept, p.amount))
    return out


def res_header():
    return "reservation,guest,member,room,rate,arrival,nights,status,booked,cancelled"


def post_header():
    return "reservation,date,dept,amount"


def encode_reservations(res):
    lines = [res_header()]
    for rv in res:
        c = "-" if rv.cancelled is None else date_of(rv.cancelled)
        lines.append(f"{rv.rid},{rv.guest},{rv.member},{rv.room},{rv.rate},{date_of(rv.arrival)},{rv.nights},"
                     f"{rv.status},{date_of(rv.booked)},{c}")
    return "\n".join(lines) + "\n"


def encode_postings(posts):
    lines = [post_header()]
    for p in posts:
        lines.append(f"{p.rid},{date_of(p.day)},{p.dept},{dollars(p.amount)}")
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

def rid_ok(t, code):
    return len(t) == 7 and t[0] == code and is_digits(t[1:])


def body_lines(path, text, header):
    lines = text.split("\n")
    if len(lines) < 2 or lines[-1] != "":
        raise BatchError(f"{path}: no final newline")
    if lines[0] != header:
        raise BatchError(f"{path} line 1: bad header")
    return lines[1:-1]


def parse_reservations(path, ht, text):
    out = []
    for k, line in enumerate(body_lines(path, text, res_header())):
        def bad(what):
            return BatchError(f"{path} line {k + 2}: {what}")
        f = line.split(",")
        if len(f) != 10:
            raise bad(f"expected 10 fields, found {len(f)}")
        if not rid_ok(f[0], ht.code):
            raise bad(f"bad reservation '{f[0]}'")
        if f[1] == "":
            raise bad("empty guest name")
        if f[2] != "-" and not (len(f[2]) == 5 and f[2][0] == "M" and is_digits(f[2][1:])):
            raise bad(f"bad member '{f[2]}'")
        if f[3] not in ROOM_TYPES:
            raise bad(f"bad room type '{f[3]}'")
        if f[4] not in RATE_CODES:
            raise bad(f"unknown rate code '{f[4]}'")
        arrival = day_of(f[5])
        if arrival is None:
            raise bad(f"bad arrival '{f[5]}'")
        if not is_digits(f[6]) or len(f[6]) > 2 or not 1 <= int(f[6]) <= 14:
            raise bad(f"bad nights '{f[6]}'")
        if f[7] not in ("B", "X", "N"):
            raise bad(f"bad status '{f[7]}'")
        booked = day_of(f[8])
        if booked is None or booked >= arrival:
            raise bad(f"bad booking date '{f[8]}'")
        cancelled = None
        if f[9] != "-":
            cancelled = day_of(f[9])
            if cancelled is None or cancelled < booked or cancelled > arrival:
                raise bad(f"bad cancellation date '{f[9]}'")
        if (f[7] == "X") != (cancelled is not None):
            raise bad("status and cancellation date disagree")
        out.append(Res(f[0], f[1], f[2], f[3], f[4], arrival, int(f[6]), f[7], booked, cancelled))
    return out


def parse_postings(path, ht, text):
    out = []
    for k, line in enumerate(body_lines(path, text, post_header())):
        def bad(what):
            return BatchError(f"{path} line {k + 2}: {what}")
        f = line.split(",")
        if len(f) != 4:
            raise bad(f"expected 4 fields, found {len(f)}")
        if not rid_ok(f[0], ht.code):
            raise bad(f"bad reservation '{f[0]}'")
        day = day_of(f[1])
        if day is None:
            raise bad(f"bad date '{f[1]}'")
        if f[2] not in DEPTS:
            raise bad(f"unknown department '{f[2]}'")
        amount = cents_of(f[3])
        if amount is None or amount == 0:
            raise bad(f"bad amount '{f[3]}'")
        out.append(Post(f[0], day, f[2], amount))
    return out


def same(a, b, keys):
    return len(a) == len(b) and all(all(getattr(x, k) == getattr(y, k) for k in keys) for x, y in zip(a, b))


# ------------------------------------------------------------ the audit

def night_rate(ht, rv, day):
    code = RATE_CODES.index(rv.rate)
    rate = ht.rack[ROOM_TYPES.index(rv.room)] * RATE_PCT[code] // 100
    if RATE_WEEKEND[code] and weekday(day) in (4, 5):
        rate += rate * WEEKEND_PCT // 100
    return rate


class Audit:
    def __init__(self):
        self.sold = [[0] * 3 for _ in range(NIGHTS)]
        self.revenue = [0] * NIGHTS
        self.walked = []        # reservation indexes, in walk order
        self.oversold = 0
        self.by_segment = [0, 0, 0, 0]
        self.noshow_fees = 0
        self.noshows = 0
        self.late_fees = 0
        self.late = 0
        self.free_cancels = 0
        self.commission = 0
        self.tax = 0
        self.fees = 0
        self.stay_revenue = {}  # reservation index -> room revenue of the whole stay
        self.folio = {}         # reservation id -> [charges, payments]
        self.incidentals = [0] * 5
        self.suspense = []      # postings to no open folio


def audit(ht, res, posts):
    au = Audit()
    walked = set()
    for night in range(NIGHTS):
        for t in range(3):
            house = [i for i, rv in enumerate(res)
                     if rv.status == "B" and rv.room == ROOM_TYPES[t] and i not in walked
                     and rv.arrival <= night < rv.arrival + rv.nights]
            over = len(house) - ht.rooms[t]
            if over > 0:
                # walk tonight's arrivals that booked last
                arriving = [i for i in house if res[i].arrival == night]
                arriving.sort(key=lambda i: (res[i].booked, res[i].rid), reverse=True)
                for i in arriving[:over]:
                    walked.add(i)
                    au.walked.append(i)
                if over > len(arriving):
                    au.oversold += over - len(arriving)
                house = [i for i in house if i not in walked]
            au.sold[night][t] = min(len(house), ht.rooms[t])
            for i in house:
                rate = night_rate(ht, res[i], night)
                au.revenue[night] += rate
                au.by_segment[SEGMENT_NAMES.index(SEGMENTS[RATE_CODES.index(res[i].rate)])] += rate
                if res[i].rate == "OTA":
                    au.commission += rate * OTA_COMMISSION_BP // 10000
    for i, rv in enumerate(res):
        if rv.status == "N" and 0 <= rv.arrival < NIGHTS:
            au.noshows += 1
            au.noshow_fees += night_rate(ht, rv, rv.arrival)
        if rv.status == "X" and 0 <= rv.arrival < NIGHTS:
            if rv.arrival - rv.cancelled < FREE_CANCEL_DAYS:
                au.late += 1
                au.late_fees += night_rate(ht, rv, rv.arrival)
            else:
                au.free_cancels += 1
    room_nights = sum(sum(s) for s in au.sold)
    taxable = sum(au.revenue) + au.noshow_fees + au.late_fees
    au.tax = (taxable * ht.tax_bp + 5000) // 10000
    au.fees = room_nights * ht.fee
    for i, rv in enumerate(res):
        if rv.status != "B" or i in walked:
            continue
        stay = 0
        for d in range(rv.arrival, rv.arrival + rv.nights):
            stay += night_rate(ht, rv, d)
        au.stay_revenue[i] = stay
        tax = (stay * ht.tax_bp + 5000) // 10000 + rv.nights * ht.fee
        au.folio[rv.rid] = [stay + tax, 0]
    for p in posts:
        f = au.folio.get(p.rid)
        if f is None:
            au.suspense.append(p)
            continue
        if p.dept == "PY":
            f[1] += p.amount
        else:
            f[0] += p.amount
            au.incidentals[DEPTS.index(p.dept)] += p.amount
    return au


# ------------------------------------------------------------ reports

def occupancy_report(ht, au):
    avail = sum(ht.rooms) * NIGHTS
    sold = sum(sum(s) for s in au.sold)
    peak = 0
    for n in range(NIGHTS):
        if sum(au.sold[n]) > sum(au.sold[peak]):
            peak = n
    rev = sum(au.revenue)
    adr = rev // sold if sold else 0
    revpar = rev // avail
    return (f"{ht.name} occupancy: {pct1(sold, avail)} ({group3(sold)} of {group3(avail)} room-nights); "
            f"peak {night_name(peak)} {pct1(sum(au.sold[peak]), sum(ht.rooms))}; ADR {money(adr)}, RevPAR {money(revpar)}; "
            f"{len(au.walked)} walked for {money(len(au.walked) * WALK_COST)}, {au.oversold} oversold")


def revenue_report(ht, au):
    rev = sum(au.revenue)
    segs = ", ".join(f"{SEGMENT_NAMES[k]} {money(au.by_segment[k])}" for k in range(4))
    net = rev + au.noshow_fees + au.late_fees - au.commission - len(au.walked) * WALK_COST
    return (f"{ht.name} revenue: rooms {money(rev)} ({segs}); {au.noshows} no-shows {money(au.noshow_fees)}, "
            f"{au.late} late cancels {money(au.late_fees)}, {au.free_cancels} free; OTA commission {money(au.commission)}; "
            f"net {money(net)}")


def tax_report(ht, au):
    by_night = max(range(NIGHTS), key=lambda n: (au.revenue[n], -n))
    return (f"{ht.name} tax: city tax {ht.tax_bp // 100}.{ht.tax_bp % 100:02}% {money(au.tax)}, "
            f"occupancy fee {money(au.fees)}, remit {money(au.tax + au.fees)}; "
            f"best night {night_name(by_night)} {money(au.revenue[by_night])}")


def incidental_report(ht, au):
    total = sum(au.incidentals)
    top = 0
    for k in range(5):
        if au.incidentals[k] > au.incidentals[top]:
            top = k
    sus = sum(p.amount for p in au.suspense)
    first = "none"
    if au.suspense:
        p = au.suspense[0]
        first = f"first {p.rid} {p.dept} {money(p.amount)}"
    return (f"{ht.name} incidentals: {money(total)}; top {DEPT_NAMES[top]} {money(au.incidentals[top])}; "
            f"{len(au.suspense)} to suspense {money(sus)} ({first})")


def folio_report(ht, res, au):
    out = 0
    due = 0
    credit = 0
    house = 0
    big = None
    for i, rv in enumerate(res):
        f = au.folio.get(rv.rid)
        if f is None:
            continue
        if rv.arrival + rv.nights > NIGHTS - 1:
            house += 1
            continue
        out += 1
        bal = f[0] - f[1]
        if bal >= 0:
            due += bal
        else:
            credit -= bal
        if big is None or bal > big[0]:
            big = (bal, rv)
    tail = f"largest {big[1].rid} {big[1].guest} {money(big[0])}" if big else "none"
    return (f"{ht.name} folios: {group3(out)} checked out, {money(due)} due, {money(credit)} in credit; "
            f"{group3(house)} still in house; {tail}")


# ------------------------------------------------------------ the loyalty ledger

class Ledger:
    def __init__(self):
        self.points = {}
        self.tier = {}
        self.log = []
        self.upgrades = []
        self.issued = 0


def opening(member):
    n = int(member[1:])
    return hash32(n * 7919 + 13) % 16000


def tier_of(points):
    t = 0
    for k in range(4):
        if points >= TIER_AT[k]:
            t = k
    return t


def post_stays(lg, ht, res, au):
    stays = [i for i in au.stay_revenue if res[i].member != "-" and res[i].arrival + res[i].nights <= NIGHTS - 1]
    stays.sort(key=lambda i: (res[i].arrival + res[i].nights, res[i].rid))
    issued = 0
    ups = 0
    for i in stays:
        rv = res[i]
        m = rv.member
        if m not in lg.points:
            lg.points[m] = opening(m)
            lg.tier[m] = tier_of(lg.points[m])
        base = au.stay_revenue[i] // 10
        pts = base * TIER_PCT[lg.tier[m]] // 100
        lg.points[m] += pts
        issued += pts
        lg.log.append(f"{m} {rv.rid} {date_of(rv.arrival + rv.nights)} +{pts} {TIERS[lg.tier[m]]}")
        t = tier_of(lg.points[m])
        while lg.tier[m] < t:
            lg.tier[m] += 1
            if lg.tier[m] == 2:
                lg.points[m] += GOLD_BONUS
                issued += GOLD_BONUS
                lg.log.append(f"{m} gold bonus +{GOLD_BONUS}")
            if lg.tier[m] >= 2:
                lg.upgrades.append(f"{m} to {TIERS[lg.tier[m]]} at {ht.name} with {group3(lg.points[m])} points")
            ups += 1
            t = tier_of(lg.points[m])
    lg.issued += issued
    return f"{ht.name} loyalty: {len(stays)} stays posted for {group3(issued)} points; {ups} upgrades"


# ------------------------------------------------------------ the run

def run():
    hotels = [
        Hotel("albany", "A", [90, 70, 20], [15900, 14900, 32900], 23, 1475, 250),
        Hotel("bristol", "B", [60, 48, 12], [18900, 17500, 38500], 41, 1200, 0),
        Hotel("carmel", "C", [40, 34, 16], [26500, 24900, 52000], 59, 1050, 300),
        Hotel("denver", "D", [120, 96, 24], [12900, 11900, 27500], 73, 1575, 200),
    ]
    rs = [build_reservations(h) for h in hotels]
    ps = [build_postings(h, r) for h, r in zip(hotels, rs)]
    for h, r in zip(hotels, rs):
        write_text(f"{h.name}_reservations.csv", encode_reservations(r))
    for h, p in zip(hotels, ps):
        write_text(f"{h.name}_postings.csv", encode_postings(p))

    whole = read_text("albany_reservations.csv")
    write_text("albany_reservations.bad", damage(whole, 300, 5, "2026-10-32"))

    tr = [read_text(f"{h.name}_reservations.csv") for h in hotels]
    tp = [read_text(f"{h.name}_postings.csv") for h in hotels]
    for h, a, b in zip(hotels, tr, tp):
        print(f"{h.name}: reservations {group3(len(a))} bytes, fnv {text_sum(a):08x}; postings {group3(len(b))} bytes, fnv {text_sum(b):08x}")

    gr = [parse_reservations(f"{h.name}_reservations.csv", h, t) for h, t in zip(hotels, tr)]
    gp = [parse_postings(f"{h.name}_postings.csv", h, t) for h, t in zip(hotels, tp)]
    for path, ht, text, posting in [("albany_reservations.bad", hotels[0], read_text("albany_reservations.bad"), False),
                                    ("carmel_postings.csv", hotels[3], tp[2], True)]:
        try:
            if posting:
                parse_postings(path, ht, text)
            else:
                parse_reservations(path, ht, text)
            print(f"{path}: parsed")
        except BatchError as e:
            print(e)
    ok = all(same(a, b, RES_KEYS) for a, b in zip(gr, rs)) and all(same(a, b, POST_KEYS) for a, b in zip(gp, ps))
    print(f"parsed files match the built ones: {'true' if ok else 'false'}")

    au = [audit(h, r, p) for h, r, p in zip(hotels, gr, gp)]

    lines = []
    for h, a in zip(hotels, au):
        lines.append(occupancy_report(h, a))
    for h, a in zip(hotels, au):
        lines.append(revenue_report(h, a))
    for h, a in zip(hotels, au):
        lines.append(tax_report(h, a))
    for h, a in zip(hotels, au):
        lines.append(incidental_report(h, a))
    for h, r, a in zip(hotels, gr, au):
        lines.append(folio_report(h, r, a))
    for line in lines:
        print(line)

    lg = Ledger()
    order = list(range(4))
    if os.environ.get("HOTEL_REVERSE") == "1":
        order.reverse()
    for k in order:
        print(post_stays(lg, hotels[k], gr[k], au[k]))
    for u in lg.upgrades:
        print(u)
    top = sorted(lg.points, key=lambda m: (-lg.points[m], m))[:5]
    print("top members: " + ", ".join(f"{m} {group3(lg.points[m])} {TIERS[lg.tier[m]]}" for m in top))
    counts = [0, 0, 0, 0]
    for m in lg.tier:
        counts[lg.tier[m]] += 1
    print(f"tiers: " + ", ".join(f"{counts[k]} {TIERS[k]}" for k in range(4)) + f"; {group3(lg.issued)} points issued")
    write_text("loyalty.log", "\n".join(lg.log) + "\n")
    back = read_text("loyalty.log")
    rows = len(back.split("\n")) - 1
    print(f"loyalty.log: {group3(rows)} entries, {group3(len(back))} bytes, fnv {text_sum(back):08x}")

    for h in hotels:
        os.remove(f"{h.name}_reservations.csv")
        os.remove(f"{h.name}_postings.csv")
    os.remove("albany_reservations.bad")
    os.remove("loyalty.log")


if __name__ == "__main__":
    try:
        run()
    except BatchError as e:
        print(f"hotel_audit failed: {e}")
