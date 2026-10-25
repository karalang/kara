"""The Python mirror of grain_intake.kara: a farmers' grain co-op's harvest
week at four country elevators. It writes expected.out.

Run: python3 -I -B grain_intake.py > expected.out
     GRAIN_REVERSE=1 python3 -I -B grain_intake.py   # the elevators ask for rail cars in reverse"""
import os

MASK32 = 4294967295
N_GROWERS = 80
N_TICKETS = 420
DAYS = 7
CROPS = ["CORN", "SOY", "WHEAT"]
CROP_WORDS = ["corn", "soy", "wheat"]
# pounds in a bushel
LB_PER_BU = [56, 60, 60]
# moisture in tenths of a percent: the base the co-op pays on, and the
# most it takes
BASE_MOIST = [150, 130, 135]
MAX_MOIST = [300, 180, 200]
# shrink in hundredths of a percent for each tenth of a point over the
# base, plus the handling shrink on any grain that is dried
SHRINK = [14, 13, 13]
HANDLING = 50
# drying, in tenths of a cent a bushel for each half point over the base
DRY_RATE = [25, 30, 30]
# foreign material over the limit is docked from the weight
FM_LIMIT = [30, 20, 10]
# discounts in cents a bushel for each point (or part) of damage over the
# limit and each pound (or part) of test weight under the minimum
DMG_LIMIT = [50, 30, 40]
DMG_RATE = [3, 4, 5]
TW_MIN = [540, 540, 580]
TW_RATE = [2, 3, 3]
# the US grade: the first grade whose test weight minimum, damage limit
# and foreign material limit the grain meets; past grade 5 it is sample
# grade, at a further discount
GRADE_TW = [[560, 540, 520, 490, 460], [560, 540, 520, 490, 460], [600, 580, 560, 540, 510]]
GRADE_DMG = [[30, 50, 70, 100, 150], [20, 30, 50, 80, 120], [20, 40, 70, 100, 150]]
GRADE_FM = [[20, 30, 40, 50, 70], [10, 20, 30, 50, 70], [4, 7, 13, 30, 50]]
SAMPLE_GRADE = 6
SAMPLE_DISC = 20
# the checkoff: tenths of a cent a bushel for corn and wheat, and for
# soybeans hundredths of a percent of the value
CHECKOFF = [10, 50, 20]
NONMEMBER_FEE = 3
# the board price in cents a bushel, by crop and day
BOARD = [[412, 418, 409, 421, 426, 419, 415],
         [1012, 1020, 1004, 998, 1011, 1026, 1018],
         [548, 552, 561, 557, 549, 544, 553]]
BINS = [5, 3, 2]
FIRST = ["Ada", "Bert", "Cora", "Dale", "Edna", "Floyd", "Gail", "Hank", "Ida", "Jules", "Kurt",
         "Lena", "Mabel", "Ned", "Opal", "Pete", "Ray", "Ruth", "Stan", "Tess", "Vern", "Walt",
         "Wilma", "Zeke"]
LAST = ["Anders", "Brandt", "Carlson", "Dahl", "Ekstrom", "Fenske", "Gruber", "Halvorsen", "Iverson",
        "Jansen", "Kohler", "Lund", "Meyer", "Nygaard", "Olson", "Peterson", "Quam", "Reimer",
        "Schultz", "Thorson", "Ulrich", "Vogel", "Wagner", "Zimmer"]
# The railroad's hopper cars for the week, granted in the order the
# elevators ask.
RAIL_CARS = 70
CAR_BU = 3500


class Elevator:
    def __init__(self, code, name, digit, seed, under, wet, corn, soy):
        self.code = code
        self.name = name
        self.digit = digit
        self.seed = seed
        self.under = under
        self.wet = wet
        self.corn = corn
        self.soy = soy


class Grower:
    def __init__(self, gid, last, first, member):
        self.gid = gid
        self.last = last
        self.first = first
        self.member = member

    def key(self):
        return (self.gid, self.last, self.first, self.member)


class Bin:
    def __init__(self, bin, crop, capacity, start):
        self.bin = bin
        self.crop = crop
        self.capacity = capacity
        self.start = start

    def key(self):
        return (self.bin, self.crop, self.capacity, self.start)


class Contract:
    def __init__(self, cid, gid, crop, bushels, price, from_day, to_day):
        self.cid = cid
        self.gid = gid
        self.crop = crop
        self.bushels = bushels
        self.price = price
        self.from_day = from_day
        self.to_day = to_day

    def key(self):
        return (self.cid, self.gid, self.crop, self.bushels, self.price, self.from_day, self.to_day)


class Ticket:
    def __init__(self, tno, gid, crop, truck, day, gross, tare, moist, tw, fm, dmg):
        self.tno = tno
        self.gid = gid
        self.crop = crop
        self.truck = truck
        self.day = day
        self.gross = gross
        self.tare = tare
        self.moist = moist
        self.tw = tw
        self.fm = fm
        self.dmg = dmg

    def key(self):
        return (self.tno, self.gid, self.crop, self.truck, self.day, self.gross, self.tare, self.moist,
                self.tw, self.fm, self.dmg)


class Intake:
    pass


class Rail:
    def __init__(self):
        self.cars = RAIL_CARS
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
    return "$%s.%02d" % (group3(c // 100), c % 100)


def dollars(c):
    return "%d.%02d" % (c // 100, c % 100)


def bushels(v):
    return "%s.%02d" % (group3(v // 100), v % 100)


def tenths(v):
    return "%d.%d" % (v // 10, v % 10)


def is_digits(s):
    return len(s) > 0 and all("0" <= ch <= "9" for ch in s)


def small_int(t, lo, hi):
    if not is_digits(t) or len(t) > 6:
        return None
    v = int(t)
    if v < lo or v > hi:
        return None
    return v


def decimal_of(t, places):
    dot = t.find(".")
    if dot < 1 or len(t) - dot != places + 1:
        return None
    a = t[:dot]
    b = t[dot + 1:]
    if not (is_digits(a) and is_digits(b)) or len(a) > 3:
        return None
    return int(a) * (10 ** places) + int(b)


def name_ok(t):
    return len(t) > 0 and all(("A" <= ch <= "Z") or ("a" <= ch <= "z") for ch in t)


def file_line(name, text):
    rows = len(text.split("\n")) - 2
    return "%s: %s rows, %s bytes, fnv %08x" % (name, group3(rows), group3(len(text)), text_sum(text))


def crop_of(name):
    for c in range(3):
        if CROPS[c] == name:
            return c
    return -1


def spot(elev, crop, day):
    return BOARD[crop][day - 1] - elev.under


def grade_of(crop, tw, dmg, fm):
    for g in range(5):
        if tw >= GRADE_TW[crop][g] and dmg <= GRADE_DMG[crop][g] and fm <= GRADE_FM[crop][g]:
            return g + 1
    return SAMPLE_GRADE


def grade_name(g):
    return "S" if g == SAMPLE_GRADE else str(g)


# ------------------------------------------------------------ building

def build_growers(elev):
    out = []
    for k in range(N_GROWERS):
        r = mix(elev.seed, k, 11)
        out.append(Grower(elev.digit * 1000 + k + 1, LAST[mix(elev.seed, k, 12) % 24],
                          FIRST[mix(elev.seed, k, 13) % 24], r % 100 < 85))
    return out


def build_bins(elev):
    out = []
    n = 0
    for c in range(3):
        for j in range(BINS[c]):
            r = mix(elev.seed, n, 21)
            capacity = (20 + r % 31) * 1000
            start = capacity * ((r >> 8) % 30) // 100
            out.append(Bin("%s-B%02d" % (elev.code, n + 1), CROPS[c], capacity, start))
            n += 1
    return out


def crop_pick(elev, r):
    p = r % 100
    if p < elev.corn:
        return 0
    if p < elev.corn + elev.soy:
        return 1
    return 2


def build_contracts(elev, growers):
    out = []
    n = 0
    for k in range(len(growers)):
        r = mix(elev.seed, k, 31)
        for j in range(r % 4):
            q = mix(elev.seed, k * 8 + j, 32)
            crop = crop_pick(elev, q)
            from_day = 1 + (q >> 8) % 3
            to_day = min(DAYS, from_day + 2 + (q >> 10) % 4)
            price = BOARD[crop][0] - elev.under + (q >> 13) % 41 - 20
            out.append(Contract(elev.digit * 10000 + n + 1, growers[k].gid, crop, 500 + ((q >> 18) % 9) * 250,
                                price, from_day, to_day))
            n += 1
    return out


def build_tickets(elev, growers):
    out = []
    for n in range(N_TICKETS):
        r = mix(elev.seed, n, 41)
        q = mix(elev.seed, n, 42)
        g = growers[mix(elev.seed, n, 43) % N_GROWERS]
        crop = crop_pick(elev, q)
        if (r >> 4) % 100 < 15:
            tare = 12000 + r % 3000
            net = 16000 + (r >> 8) % 8000
        else:
            tare = 28000 + r % 4000
            net = 44000 + (r >> 8) % 12000
        if crop == 0:
            moist = 140 + (q >> 8) % 120 + elev.wet
            if (q >> 24) % 50 == 0:
                moist = 305 + (q >> 8) % 40
            tw = 500 + (q >> 16) % 80
            fm = (q >> 4) % 45
        elif crop == 1:
            moist = 100 + (q >> 8) % 85 + elev.wet // 3
            tw = 520 + (q >> 16) % 60
            fm = (q >> 4) % 35
        else:
            moist = 110 + (q >> 8) % 75
            tw = 560 + (q >> 16) % 60
            fm = (q >> 4) % 20
        d = mix(elev.seed, n, 44)
        dmg = d % 50 + ((d >> 8) % 70 if (d >> 16) % 10 == 0 else 0)
        out.append(Ticket(elev.digit * 100000 + 1001 + n, g.gid, crop, "T%d" % (100 + (r >> 20) % 900),
                          1 + n * DAYS // N_TICKETS, tare + net, tare, moist, tw, fm, dmg))
    return out


# ------------------------------------------------------------ files

GROWER_HEADER = "grower,last,first,member"
BIN_HEADER = "bin,crop,capacity_bu,start_bu"
CONTRACT_HEADER = "contract,grower,crop,bushels,price,from_day,to_day"
TICKET_HEADER = "ticket,grower,crop,truck,day,gross_lb,tare_lb,moisture,test_weight,fm,damage"


def encode_growers(growers):
    out = GROWER_HEADER + "\n"
    for g in growers:
        out += "%d,%s,%s,%s\n" % (g.gid, g.last, g.first, "Y" if g.member else "N")
    return out


def encode_bins(bins):
    out = BIN_HEADER + "\n"
    for b in bins:
        out += "%s,%s,%d,%d\n" % (b.bin, b.crop, b.capacity, b.start)
    return out


def encode_contracts(contracts):
    out = CONTRACT_HEADER + "\n"
    for c in contracts:
        out += "%d,%d,%s,%d,%s,%d,%d\n" % (c.cid, c.gid, CROPS[c.crop], c.bushels, dollars(c.price), c.from_day,
                                           c.to_day)
    return out


def encode_tickets(tickets):
    out = TICKET_HEADER + "\n"
    for t in tickets:
        out += "%d,%d,%s,%s,%d,%d,%d,%s,%s,%s,%s\n" % (t.tno, t.gid, CROPS[t.crop], t.truck, t.day, t.gross, t.tare,
                                                       tenths(t.moist), tenths(t.tw), tenths(t.fm), tenths(t.dmg))
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


def tenths_field(path, n, what, t, lo, hi):
    v = decimal_of(t, 1)
    if v is None or v < lo or v > hi:
        raise BatchError("%s line %d: bad %s '%s'" % (path, n, what, t))
    return v


def crop_field(path, n, t):
    c = crop_of(t)
    if c < 0:
        raise BatchError("%s line %d: bad crop '%s'" % (path, n, t))
    return c


def parse_growers(path, elev, text):
    body = body_lines(path, text, GROWER_HEADER)
    out = []
    seen = set()
    for k in range(len(body)):
        n = k + 2
        f = fields_of(path, n, body[k], 4)
        gid = small_int(f[0], 1000, 9999)
        if gid is None or len(f[0]) != 4:
            raise BatchError("%s line %d: bad grower '%s'" % (path, n, f[0]))
        if gid // 1000 != elev.digit:
            raise BatchError("%s line %d: grower %d is not at %s" % (path, n, gid, elev.code))
        if gid in seen:
            raise BatchError("%s line %d: grower %d appears twice" % (path, n, gid))
        seen.add(gid)
        if not name_ok(f[1]) or not name_ok(f[2]):
            raise BatchError("%s line %d: bad name '%s %s'" % (path, n, f[2], f[1]))
        if f[3] != "Y" and f[3] != "N":
            raise BatchError("%s line %d: bad member '%s'" % (path, n, f[3]))
        out.append(Grower(gid, f[1], f[2], f[3] == "Y"))
    return out


def bin_id_ok(t):
    return len(t) == 7 and t[3:5] == "-B" and is_digits(t[5:7]) and all("A" <= ch <= "Z" for ch in t[0:3])


def parse_bins(path, elev, text):
    body = body_lines(path, text, BIN_HEADER)
    out = []
    seen = set()
    for k in range(len(body)):
        n = k + 2
        f = fields_of(path, n, body[k], 4)
        b = f[0]
        if not bin_id_ok(b):
            raise BatchError("%s line %d: bad bin '%s'" % (path, n, b))
        if b[0:3] != elev.code:
            raise BatchError("%s line %d: bin %s is not at %s" % (path, n, b, elev.code))
        if b in seen:
            raise BatchError("%s line %d: bin %s appears twice" % (path, n, b))
        seen.add(b)
        crop_field(path, n, f[1])
        capacity = int_field(path, n, "capacity", f[2], 1, 999999)
        start = int_field(path, n, "start", f[3], 0, 999999)
        if start > capacity:
            raise BatchError("%s line %d: bin %s starts over its capacity" % (path, n, b))
        out.append(Bin(b, f[1], capacity, start))
    return out


def parse_contracts(path, elev, growers, text):
    body = body_lines(path, text, CONTRACT_HEADER)
    ids = set(g.gid for g in growers)
    out = []
    seen = set()
    for k in range(len(body)):
        n = k + 2
        f = fields_of(path, n, body[k], 7)
        cid = small_int(f[0], 10000, 99999)
        if cid is None or len(f[0]) != 5:
            raise BatchError("%s line %d: bad contract '%s'" % (path, n, f[0]))
        if cid // 10000 != elev.digit:
            raise BatchError("%s line %d: contract %d is not at %s" % (path, n, cid, elev.code))
        if cid in seen:
            raise BatchError("%s line %d: contract %d appears twice" % (path, n, cid))
        seen.add(cid)
        gid = small_int(f[1], 0, 9999)
        if gid is None or gid not in ids:
            raise BatchError("%s line %d: no grower '%s'" % (path, n, f[1]))
        crop = crop_field(path, n, f[2])
        bu = int_field(path, n, "bushels", f[3], 1, 999999)
        price = decimal_of(f[4], 2)
        if price is None or price < 100 or price > 3000:
            raise BatchError("%s line %d: bad price '%s'" % (path, n, f[4]))
        from_day = int_field(path, n, "from_day", f[5], 1, DAYS)
        to_day = int_field(path, n, "to_day", f[6], from_day, DAYS)
        out.append(Contract(cid, gid, crop, bu, price, from_day, to_day))
    return out


def parse_tickets(path, elev, growers, text):
    body = body_lines(path, text, TICKET_HEADER)
    ids = set(g.gid for g in growers)
    out = []
    last = 0
    for k in range(len(body)):
        n = k + 2
        f = fields_of(path, n, body[k], 11)
        tno = small_int(f[0], 100000, 999999)
        if tno is None or len(f[0]) != 6:
            raise BatchError("%s line %d: bad ticket '%s'" % (path, n, f[0]))
        if tno // 100000 != elev.digit:
            raise BatchError("%s line %d: ticket %d is not at %s" % (path, n, tno, elev.code))
        if tno <= last:
            raise BatchError("%s line %d: ticket %d is out of order" % (path, n, tno))
        last = tno
        gid = small_int(f[1], 0, 9999)
        if gid is None or gid not in ids:
            raise BatchError("%s line %d: no grower '%s'" % (path, n, f[1]))
        crop = crop_field(path, n, f[2])
        if len(f[3]) < 2 or f[3][0] != "T" or not is_digits(f[3][1:]):
            raise BatchError("%s line %d: bad truck '%s'" % (path, n, f[3]))
        day = int_field(path, n, "day", f[4], 1, DAYS)
        gross = int_field(path, n, "gross", f[5], 1, 199999)
        tare = int_field(path, n, "tare", f[6], 1, 199999)
        if tare >= gross:
            raise BatchError("%s line %d: ticket %d has tare %s lb over gross %s lb" % (
                path, n, tno, group3(tare), group3(gross)))
        moist = tenths_field(path, n, "moisture", f[7], 0, 400)
        tw = tenths_field(path, n, "test weight", f[8], 300, 700)
        fm = tenths_field(path, n, "fm", f[9], 0, 300)
        dmg = tenths_field(path, n, "damage", f[10], 0, 500)
        out.append(Ticket(tno, gid, crop, f[3], day, gross, tare, moist, tw, fm, dmg))
    return out


# ------------------------------------------------------------ the intake

# Grade, shrink, bin and settle every ticket, in ticket order: each takes
# the bin room and the contract bushels the ones before it left.
def close_week(elev, growers, bins, contracts, tickets):
    m = Intake()
    gidx = {}
    for i in range(len(growers)):
        gidx[growers[i].gid] = i
    m.fill = [b.start * 100 for b in bins]
    m.delivered = [0] * len(contracts)
    by_grower = [[] for _ in growers]
    for c in range(len(contracts)):
        by_grower[gidx[contracts[c].gid]].append(c)
    m.status = []
    m.net = []
    m.bu = []
    m.grade = []
    m.bin = []
    m.con_bu = []
    m.con_value = []
    m.spot_bu = []
    m.spot_value = []
    m.dry = []
    m.disc = []
    m.fee = []
    m.checkoff = []
    m.pay = []
    m.pile = 0
    m.g_pay = [0] * len(growers)
    m.g_tickets = [0] * len(growers)
    m.g_bu = [[0, 0, 0] for _ in growers]
    m.g_ded = [0] * len(growers)
    m.g_gross = [0] * len(growers)
    for t in tickets:
        c = t.crop
        gi = gidx[t.gid]
        net = t.gross - t.tare
        m.net.append(net)
        dock = net * (t.fm - FM_LIMIT[c]) // 1000 if t.fm > FM_LIMIT[c] else 0
        clean = net - dock
        over = t.moist - BASE_MOIST[c] if t.moist > BASE_MOIST[c] else 0
        shrink = over * SHRINK[c] + HANDLING if over > 0 else 0
        dry_lb = clean - clean * shrink // 10000
        bu = dry_lb * 100 // LB_PER_BU[c]
        grade = grade_of(c, t.tw, t.dmg, t.fm)
        status = 0
        where = -1
        if t.moist > MAX_MOIST[c]:
            status = 1
        else:
            for b in range(len(bins)):
                if bins[b].crop == CROPS[c] and m.fill[b] + bu <= bins[b].capacity * 100:
                    where = b
                    break
            if where >= 0:
                m.fill[where] += bu
            elif c == 0:
                m.pile += bu
            else:
                status = 2
        m.status.append(status)
        m.grade.append(grade)
        m.bin.append(where)
        if status != 0:
            for v in (m.bu, m.con_bu, m.con_value, m.spot_bu, m.spot_value, m.dry, m.disc, m.fee, m.checkoff, m.pay):
                v.append(0)
            continue
        m.bu.append(bu)
        left = bu
        con_bu = 0
        con_value = 0
        for k in by_grower[gi]:
            ct = contracts[k]
            if ct.crop != c or t.day < ct.from_day or t.day > ct.to_day:
                continue
            open_bu = ct.bushels * 100 - m.delivered[k]
            if open_bu <= 0 or left == 0:
                continue
            take = min(left, open_bu)
            m.delivered[k] += take
            con_bu += take
            con_value += take * ct.price // 100
            left -= take
        spot_value = left * spot(elev, c, t.day) // 100
        halfpts = (over + 4) // 5
        dry = bu * halfpts * DRY_RATE[c] // 1000
        per_bu = 0
        if t.tw < TW_MIN[c]:
            per_bu += (TW_MIN[c] - t.tw + 9) // 10 * TW_RATE[c]
        if t.dmg > DMG_LIMIT[c]:
            per_bu += (t.dmg - DMG_LIMIT[c] + 9) // 10 * DMG_RATE[c]
        if grade == SAMPLE_GRADE:
            per_bu += SAMPLE_DISC
        disc = bu * per_bu // 100
        fee = 0 if growers[gi].member else bu * NONMEMBER_FEE // 100
        value = con_value + spot_value
        if c == 1:
            checkoff = value * CHECKOFF[c] // 10000
        else:
            checkoff = bu * CHECKOFF[c] // 1000
        pay = value - dry - disc - fee - checkoff
        m.con_bu.append(con_bu)
        m.con_value.append(con_value)
        m.spot_bu.append(left)
        m.spot_value.append(spot_value)
        m.dry.append(dry)
        m.disc.append(disc)
        m.fee.append(fee)
        m.checkoff.append(checkoff)
        m.pay.append(pay)
        m.g_pay[gi] += pay
        m.g_tickets[gi] += 1
        m.g_bu[gi][c] += bu
        m.g_gross[gi] += value
        m.g_ded[gi] += dry + disc + fee + checkoff
    return m


STATUS = ["", "too wet", "no room"]


def encode_settlements(bins, tickets, m):
    out = "ticket,grower,crop,day,net_lb,dry_bu,grade,bin,contract_bu,spot_bu,value,drying,discounts,fees,checkoff,pay\n"
    for i in range(len(tickets)):
        t = tickets[i]
        if m.status[i] != 0:
            out += "%d,%d,%s,%d,%d,%s,,,,,,,,,,\n" % (t.tno, t.gid, CROPS[t.crop], t.day, m.net[i], STATUS[m.status[i]])
            continue
        where = "PILE" if m.bin[i] < 0 else bins[m.bin[i]].bin
        out += "%d,%d,%s,%d,%d,%s,%s,%s,%s,%s,%s,%s,%s,%s,%s,%s\n" % (
            t.tno, t.gid, CROPS[t.crop], t.day, m.net[i], bushels(m.bu[i]), grade_name(m.grade[i]), where,
            bushels(m.con_bu[i]), bushels(m.spot_bu[i]), dollars(m.con_value[i] + m.spot_value[i]), dollars(m.dry[i]),
            dollars(m.disc[i]), dollars(m.fee[i]), dollars(m.checkoff[i]), dollars(m.pay[i]))
    return out


def encode_statements(growers, m):
    out = "grower,name,member,tickets,corn_bu,soy_bu,wheat_bu,value,deductions,pay\n"
    for i in range(len(growers)):
        g = growers[i]
        if m.g_tickets[i] == 0:
            continue
        out += "%d,%s %s,%s,%d,%s,%s,%s,%s,%s,%s\n" % (
            g.gid, g.first, g.last, "Y" if g.member else "N", m.g_tickets[i], bushels(m.g_bu[i][0]),
            bushels(m.g_bu[i][1]), bushels(m.g_bu[i][2]), dollars(m.g_gross[i]), dollars(m.g_ded[i]),
            dollars(m.g_pay[i]))
    return out


# ------------------------------------------------------------ reports

def intake_report(elev, tickets, m):
    taken = 0
    wet = 0
    room = 0
    bu = [0, 0, 0]
    moist = [0, 0, 0]
    count = [0, 0, 0]
    for i in range(len(tickets)):
        t = tickets[i]
        if m.status[i] == 1:
            wet += 1
        elif m.status[i] == 2:
            room += 1
        else:
            taken += 1
            bu[t.crop] += m.bu[i]
            moist[t.crop] += t.moist
            count[t.crop] += 1
    parts = []
    for c in range(3):
        mean = moist[c] // count[c] if count[c] > 0 else 0
        parts.append("%s %s bu at %s%%" % (CROP_WORDS[c], bushels(bu[c]), tenths(mean)))
    return "%s intake: %d tickets, %d taken, %d too wet, %d turned away for room; %s" % (
        elev.code, len(tickets), taken, wet, room, ", ".join(parts))


def grade_report(elev, tickets, m):
    parts = []
    for c in range(3):
        counts = [0] * 7
        for i in range(len(tickets)):
            if tickets[i].crop == c and m.status[i] == 0:
                counts[m.grade[i]] += 1
        cells = []
        for g in range(1, 7):
            if counts[g] > 0:
                cells.append("%s:%d" % (grade_name(g), counts[g]))
        parts.append("%s %s" % (CROPS[c], " ".join(cells)))
    worst = -1
    for i in range(len(tickets)):
        if m.status[i] == 0 and (worst < 0 or tickets[i].dmg > tickets[worst].dmg):
            worst = i
    w = tickets[worst]
    return "%s grades: %s; worst damage ticket %d (%s, %s%%)" % (elev.code, "; ".join(parts), w.tno, CROPS[w.crop],
                                                                 tenths(w.dmg))


def bin_report(elev, bins, m):
    cells = []
    full = 0
    for b in range(len(bins)):
        pct = m.fill[b] * 1000 // (bins[b].capacity * 100)
        if pct >= 950:
            full += 1
        cells.append("%s %s %s%%" % (bins[b].bin[4:], bins[b].crop, tenths(pct)))
    return "%s bins: %s; %d at 95%% or more; ground pile %s bu" % (elev.code, ", ".join(cells), full,
                                                                   bushels(m.pile))


def settle_report(elev, tickets, m):
    value = 0
    dry = 0
    disc = 0
    fee = 0
    checkoff = 0
    pay = 0
    con_bu = 0
    con_value = 0
    spot_bu = 0
    spot_value = 0
    for i in range(len(tickets)):
        if m.status[i] != 0:
            continue
        value += m.con_value[i] + m.spot_value[i]
        dry += m.dry[i]
        disc += m.disc[i]
        fee += m.fee[i]
        checkoff += m.checkoff[i]
        pay += m.pay[i]
        con_bu += m.con_bu[i]
        con_value += m.con_value[i]
        spot_bu += m.spot_bu[i]
        spot_value += m.spot_value[i]
    con_avg = con_value * 100 // con_bu if con_bu > 0 else 0
    spot_avg = spot_value * 100 // spot_bu if spot_bu > 0 else 0
    return ("%s settlement: %s value, %s drying, %s discounts, %s fees, %s checkoff, %s paid; "
            "%s bu on contract at $%s, %s bu at spot at $%s") % (
        elev.code, money(value), money(dry), money(disc), money(fee), money(checkoff), money(pay),
        bushels(con_bu), dollars(con_avg), bushels(spot_bu), dollars(spot_avg))


def contract_report(elev, growers, contracts, m):
    filled = 0
    part = 0
    none = 0
    short = 0
    for k in range(len(contracts)):
        want = contracts[k].bushels * 100
        if m.delivered[k] == want:
            filled += 1
        elif m.delivered[k] > 0:
            part += 1
        else:
            none += 1
        short += want - m.delivered[k]
    order = sorted(range(len(growers)), key=lambda i: (-m.g_pay[i], growers[i].gid))
    top = []
    for i in order[:3]:
        top.append("%s %s %s" % (growers[i].first, growers[i].last, money(m.g_pay[i])))
    return "%s contracts: %d written, %d filled, %d partly, %d untouched, %s bu short; top growers %s" % (
        elev.code, len(contracts), filled, part, none, bushels(short), ", ".join(top))


# ------------------------------------------------------------ rail

# An elevator ships its ground pile and its soybeans, and asks for the
# cars that takes; the railroad grants what it has left.
def ask_rail(rail, elev, tickets, m):
    ship = m.pile
    for i in range(len(tickets)):
        if m.status[i] == 0 and tickets[i].crop == 1:
            ship += m.bu[i]
    car = CAR_BU * 100
    asked = (ship + car - 1) // car
    granted = min(asked, rail.cars)
    rail.cars -= granted
    shipped = min(ship, granted * car)
    rail.log.append("%s,%d,%d,%s,%s,%d" % (elev.code, asked, granted, bushels(shipped), bushels(ship - shipped),
                                           rail.cars))
    return "%s rail: asked %d cars, granted %d, %s bu shipped, %s bu held; %d cars left" % (
        elev.code, asked, granted, bushels(shipped), bushels(ship - shipped), rail.cars)


# ------------------------------------------------------------ the job

def run():
    elevs = [
        Elevator("ASH", "Ashby", 1, 9101, 32, 0, 60, 30),
        Elevator("BEL", "Belmont", 2, 9202, 28, 15, 50, 40),
        Elevator("CLR", "Clare", 3, 9303, 41, 25, 70, 20),
        Elevator("DUN", "Dunmore", 4, 9404, 36, 5, 45, 35),
    ]
    grs = [build_growers(e) for e in elevs]
    bns = [build_bins(e) for e in elevs]
    cts = [build_contracts(elevs[i], grs[i]) for i in range(4)]
    tks = [build_tickets(elevs[i], grs[i]) for i in range(4)]
    names = []
    for e in elevs:
        names += [e.code + "_growers.csv", e.code + "_bins.csv", e.code + "_contracts.csv", e.code + "_tickets.csv"]
    for i in range(4):
        write_text(names[i * 4], encode_growers(grs[i]))
        write_text(names[i * 4 + 1], encode_bins(bns[i]))
        write_text(names[i * 4 + 2], encode_contracts(cts[i]))
        write_text(names[i * 4 + 3], encode_tickets(tks[i]))
    whole = read_text("ASH_tickets.csv")
    write_text("ASH_tickets.bad", damage(whole, 40, 6, "90000"))
    whole2 = read_text("CLR_contracts.csv")
    write_text("CLR_contracts.bad", damage(whole2, 30, 1, "3999"))
    texts = [read_text(p) for p in names]
    labels = []
    for e in elevs:
        labels += [e.code + " growers", e.code + " bins", e.code + " contracts", e.code + " tickets"]
    for k in range(16):
        print(file_line(labels[k], texts[k]))
    pg = [parse_growers(names[i * 4], elevs[i], texts[i * 4]) for i in range(4)]
    pb = [parse_bins(names[i * 4 + 1], elevs[i], texts[i * 4 + 1]) for i in range(4)]
    pc = [parse_contracts(names[i * 4 + 2], elevs[i], pg[i], texts[i * 4 + 2]) for i in range(4)]
    pt = [parse_tickets(names[i * 4 + 3], elevs[i], pg[i], texts[i * 4 + 3]) for i in range(4)]
    try:
        parse_tickets("ASH_tickets.bad", elevs[0], pg[0], read_text("ASH_tickets.bad"))
        print("ASH_tickets.bad: parsed")
    except BatchError as e:
        print(e)
    try:
        parse_contracts("CLR_contracts.bad", elevs[2], pg[2], read_text("CLR_contracts.bad"))
        print("CLR_contracts.bad: parsed")
    except BatchError as e:
        print(e)
    try:
        parse_bins("DUN_bins.csv", elevs[1], texts[13])
        print("DUN_bins.csv: parsed")
    except BatchError as e:
        print(e)
    same = all([a.key() for a in pg[i]] == [b.key() for b in grs[i]] and
               [a.key() for a in pb[i]] == [b.key() for b in bns[i]] and
               [a.key() for a in pc[i]] == [b.key() for b in cts[i]] and
               [a.key() for a in pt[i]] == [b.key() for b in tks[i]] for i in range(4))
    print("parsed files match the built ones: %s" % ("true" if same else "false"))
    weeks = [close_week(elevs[i], pg[i], pb[i], pc[i], pt[i]) for i in range(4)]
    for i in range(4):
        write_text(elevs[i].code + "_settlements.csv", encode_settlements(pb[i], pt[i], weeks[i]))
    for i in range(4):
        write_text(elevs[i].code + "_statements.csv", encode_statements(pg[i], weeks[i]))
    for i in range(4):
        print(file_line(elevs[i].code + " settlements", read_text(elevs[i].code + "_settlements.csv")))
        print(file_line(elevs[i].code + " statements", read_text(elevs[i].code + "_statements.csv")))
    for i in range(4):
        print(intake_report(elevs[i], pt[i], weeks[i]))
    for i in range(4):
        print(grade_report(elevs[i], pt[i], weeks[i]))
    for i in range(4):
        print(bin_report(elevs[i], pb[i], weeks[i]))
    for i in range(4):
        print(settle_report(elevs[i], pt[i], weeks[i]))
    for i in range(4):
        print(contract_report(elevs[i], pg[i], pc[i], weeks[i]))
    rail = Rail()
    seq = [0, 1, 2, 3]
    if os.environ.get("GRAIN_REVERSE") == "1":
        seq = [3, 2, 1, 0]
    for i in seq:
        print(ask_rail(rail, elevs[i], pt[i], weeks[i]))
    write_text("rail.log", "\n".join(rail.log) + "\n")
    back = read_text("rail.log")
    rows = len(back.split("\n")) - 1
    print("rail.log: %s entries, %s bytes, fnv %08x" % (group3(rows), group3(len(back)), text_sum(back)))
    for p in names + ["ASH_tickets.bad", "CLR_contracts.bad", "rail.log"] + \
            [e.code + "_settlements.csv" for e in elevs] + [e.code + "_statements.csv" for e in elevs]:
        os.remove(p)


def main():
    try:
        run()
    except BatchError as e:
        print("grain_intake failed: %s" % e)


main()
