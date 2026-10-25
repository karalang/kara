"""The Python mirror of fare_cap.kara: a week of smart-card fares at four
transit operators. It writes expected.out.

Run: python3 -I -B fare_cap.py > expected.out
     FARE_REVERSE=1 python3 -I -B fare_cap.py   # the operators claim the subsidy in reverse"""
import os

MASK32 = 4294967295
N_CARDS = 120
DAYS = 7
TYPES = ["ADULT", "SENIOR", "STUDENT", "CHILD"]
# percent off the fare, by card type
OFF = [0, 50, 30, 75]
# a card that loads itself adds this when its balance is under the boarding fare
AUTOLOAD = 2000
# a card that does not is declined at the gate, and its rider tops it up
# by this at the machine and taps again
TOPUP = 1000
# a tap-in within this many minutes of a tap-out is a transfer
TRANSFER_MIN = 45
# a tap-out at the station tapped in at, within this many minutes, cancels
CANCEL_MIN = 2
PLACES = ["Abbey", "Bridge", "Castle", "Dock", "Elm", "Ferry", "Garden", "Harbor", "Inlet", "Junction",
          "Kings", "Lake", "Market", "North", "Oak", "Park", "Quay", "River", "South", "Tower",
          "Union", "Vale", "West", "Yard"]
# The region's reduced-fare fund for the week: it repays the operators'
# concessions in the order they claim, from what is left.
FUND = 140000


class Operator:
    def __init__(self, code, digit, seed, stations, zones, base, per_zone, peak, daycap, weekcap):
        self.code = code
        self.digit = digit
        self.seed = seed
        self.stations = stations
        self.zones = zones
        self.base = base
        self.per_zone = per_zone
        self.peak = peak
        self.daycap = daycap
        self.weekcap = weekcap


class Station:
    def __init__(self, sid, name, zone):
        self.sid = sid
        self.name = name
        self.zone = zone

    def key(self):
        return (self.sid, self.name, self.zone)


class Card:
    def __init__(self, cno, ctype, balance, autoload):
        self.cno = cno
        self.ctype = ctype
        self.balance = balance
        self.autoload = autoload

    def key(self):
        return (self.cno, self.ctype, self.balance, self.autoload)


class Tap:
    def __init__(self, tid, cno, sid, day, minute, kind):
        self.tid = tid
        self.cno = cno
        self.sid = sid
        self.day = day
        self.minute = minute
        self.kind = kind

    def key(self):
        return (self.tid, self.cno, self.sid, self.day, self.minute, self.kind)


class Journey:
    def __init__(self, card, kind, frm, to, day, start, end, fare, conc, capped, charged):
        self.card = card
        self.kind = kind
        self.frm = frm
        self.to = to
        self.day = day
        self.start = start
        self.end = end
        self.fare = fare
        self.conc = conc
        self.capped = capped
        self.charged = charged


class Week:
    pass


class Fund:
    def __init__(self):
        self.left = FUND
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
    if c < 0:
        return "-" + money(-c)
    return "$%s.%02d" % (group3(c // 100), c % 100)


def dollars(c):
    if c < 0:
        return "-" + dollars(-c)
    return "%d.%02d" % (c // 100, c % 100)


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


def cents_of(t):
    neg = t.startswith("-")
    if neg:
        t = t[1:]
    dot = t.find(".")
    if dot < 1 or len(t) - dot != 3:
        return None
    a = t[:dot]
    b = t[dot + 1:]
    if not (is_digits(a) and is_digits(b)) or len(a) > 4:
        return None
    v = int(a) * 100 + int(b)
    return -v if neg else v


def time_of(t):
    if len(t) != 5 or t[2] != ":" or not is_digits(t[:2]) or not is_digits(t[3:]):
        return None
    h = int(t[:2])
    m = int(t[3:])
    if h > 23 or m > 59:
        return None
    return h * 60 + m


def name_ok(t):
    return len(t) > 0 and all(("A" <= ch <= "Z") or ("a" <= ch <= "z") for ch in t)


def file_line(name, text):
    rows = len(text.split("\n")) - 2
    return "%s: %s rows, %s bytes, fnv %08x" % (name, group3(rows), group3(len(text)), text_sum(text))


def type_of(name):
    for t in range(4):
        if TYPES[t] == name:
            return t
    return -1


def peak(op, day, minute):
    return day <= 5 and ((420 <= minute < 570) or (960 <= minute < 1110))


def with_off(v, ctype):
    return v * (100 - OFF[ctype]) // 100


# ------------------------------------------------------------ building

def build_stations(op):
    out = []
    for k in range(op.stations):
        out.append(Station("%s-%02d" % (op.code, k + 1), PLACES[mix(op.seed, k, 11) % 24],
                           1 + k * op.zones // op.stations))
    return out


def build_cards(op):
    out = []
    for k in range(N_CARDS):
        r = mix(op.seed, k, 21)
        p = r % 100
        if p < 70:
            ctype = 0
        elif p < 82:
            ctype = 1
        elif p < 94:
            ctype = 2
        else:
            ctype = 3
        out.append(Card(op.digit * 10000000 + 1000 + k * 7 + (r >> 20) % 7, ctype, 500 + (r >> 8) % 5000,
                        (r >> 12) % 100 < 40))
    return out


def other(ns, a, r):
    return (a + 1 + r % (ns - 1)) % ns


def build_taps(op, stations, cards):
    ns = len(stations)
    raw = []

    def add(day, minute, k, s, kind):
        raw.append((day, minute, cards[k].cno, len(raw), stations[s].sid, kind))

    for k in range(len(cards)):
        prof = mix(op.seed, k, 31) % 100
        home = mix(op.seed, k, 32) % ns
        work = other(ns, home, mix(op.seed, k, 33))
        n = 0
        for d in range(1, DAYS + 1):
            r = mix(op.seed, k * 8 + d, 35)
            trips = []
            if prof < 45:
                if d <= 5:
                    trips.append((420 + r % 150, home, work))
                    trips.append((960 + (r >> 8) % 150, work, home))
                elif (r >> 16) % 100 < 30:
                    a = 600 + (r >> 8) % 240
                    dest = other(ns, home, mix(op.seed, k * 8 + d, 36))
                    trips.append((a, home, dest))
                    trips.append((a + 120 + (r >> 20) % 180, dest, home))
            elif prof < 75:
                if (r >> 16) % 100 < 60:
                    for j in range(1 + r % 3):
                        q = mix(op.seed, k * 64 + d * 8 + j, 37)
                        f = (q >> 8) % ns
                        trips.append((360 + j * 360 + q % 240, f, other(ns, f, q >> 12)))
            else:
                if (r >> 16) % 100 < 20:
                    q = mix(op.seed, k * 64 + d * 8, 38)
                    f = (q >> 8) % ns
                    trips.append((540 + q % 600, f, other(ns, f, q >> 12)))
            for (a, f, t) in trips:
                q = mix(op.seed, k * 512 + n, 39)
                n += 1
                hops = abs(f - t)
                odd = (q >> 8) % 100
                if odd < 2:
                    # tapped in and straight back out
                    add(d, a, k, f, "IN")
                    add(d, a + 1 + (q >> 16) % 2, k, f, "OUT")
                elif odd < 5:
                    # never tapped out
                    add(d, a, k, f, "IN")
                elif odd >= 85 and hops >= 2:
                    # changed at the station halfway
                    x = (f + t) // 2
                    add(d, a, k, f, "IN")
                    m1 = a + 4 + 3 * abs(f - x)
                    add(d, m1, k, x, "OUT")
                    m2 = m1 + 3 + (q >> 16) % 15
                    add(d, m2, k, x, "IN")
                    add(d, m2 + 4 + 3 * abs(x - t), k, t, "OUT")
                else:
                    end = a + 4 + 3 * hops + q % 5
                    add(d, a, k, f, "IN")
                    add(d, end, k, t, "OUT")
                    if odd < 7:
                        # tapped out a second time
                        add(d, end + 5 + (q >> 16) % 20, k, t, "OUT")
    raw.sort(key=lambda e: (e[0], e[1], e[2], e[3]))
    out = []
    for i in range(len(raw)):
        e = raw[i]
        out.append(Tap(op.digit * 1000000 + i + 1, e[2], e[4], e[0], e[1], e[5]))
    return out


# ------------------------------------------------------------ files

STATION_HEADER = "station,name,zone"
CARD_HEADER = "card,type,balance,autoload"
TAP_HEADER = "tap,card,station,day,time,kind"
JOURNEY_HEADER = "card,kind,from,to,day,start,end,fare,concession,capped,charged"
STATEMENT_HEADER = "card,type,journeys,charged,autoloads,declined,topups,start_balance,end_balance"


def yes_no(b):
    return "Y" if b else "N"


def encode_stations(stations):
    out = STATION_HEADER + "\n"
    for s in stations:
        out += "%s,%s,%d\n" % (s.sid, s.name, s.zone)
    return out


def encode_cards(cards):
    out = CARD_HEADER + "\n"
    for c in cards:
        out += "%d,%s,%s,%s\n" % (c.cno, TYPES[c.ctype], dollars(c.balance), yes_no(c.autoload))
    return out


def encode_taps(taps):
    out = TAP_HEADER + "\n"
    for t in taps:
        out += "%d,%d,%s,%d,%s,%s\n" % (t.tid, t.cno, t.sid, t.day, clock(t.minute), t.kind)
    return out


def encode_journeys(week):
    out = JOURNEY_HEADER + "\n"
    for j in week.journeys:
        end = "-" if j.end < 0 else clock(j.end)
        out += "%d,%s,%s,%s,%d,%s,%s,%s,%s,%s,%s\n" % (j.card, j.kind, j.frm, j.to, j.day, clock(j.start), end,
                                                       dollars(j.fare), dollars(j.conc), dollars(j.capped),
                                                       dollars(j.charged))
    return out


def encode_statements(cards, week):
    out = STATEMENT_HEADER + "\n"
    for k in range(len(cards)):
        c = cards[k]
        out += "%d,%s,%d,%s,%d,%d,%d,%s,%s\n" % (c.cno, TYPES[c.ctype], week.c_journeys[k],
                                                 dollars(week.c_charged[k]), week.c_loads[k], week.c_declined[k],
                                                 week.c_topups[k], dollars(c.balance), dollars(week.balance[k]))
    return out


def write_text(path, text):
    with open(path, "w") as f:
        f.write(text)


def read_text(path):
    try:
        with open(path) as f:
            return f.read()
    except OSError:
        raise BatchError("cannot read %s" % path)


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


def station_id_ok(t):
    return len(t) == 6 and t[3] == "-" and is_digits(t[4:]) and all("A" <= ch <= "Z" for ch in t[:3])


def parse_stations(path, op, text):
    body = body_lines(path, text, STATION_HEADER)
    out = []
    seen = set()
    for k in range(len(body)):
        n = k + 2
        f = fields_of(path, n, body[k], 3)
        s = f[0]
        if not station_id_ok(s):
            raise BatchError("%s line %d: bad station '%s'" % (path, n, s))
        if s[:3] != op.code:
            raise BatchError("%s line %d: station %s is not at %s" % (path, n, s, op.code))
        if s in seen:
            raise BatchError("%s line %d: station %s appears twice" % (path, n, s))
        seen.add(s)
        if not name_ok(f[1]):
            raise BatchError("%s line %d: bad name '%s'" % (path, n, f[1]))
        zone = small_int(f[2], 1, op.zones)
        if zone is None:
            raise BatchError("%s line %d: bad zone '%s'" % (path, n, f[2]))
        out.append(Station(s, f[1], zone))
    return out


def parse_cards(path, op, text):
    body = body_lines(path, text, CARD_HEADER)
    out = []
    seen = set()
    for k in range(len(body)):
        n = k + 2
        f = fields_of(path, n, body[k], 4)
        cno = small_int(f[0], 10000000, 99999999)
        if cno is None or len(f[0]) != 8:
            raise BatchError("%s line %d: bad card '%s'" % (path, n, f[0]))
        if cno // 10000000 != op.digit:
            raise BatchError("%s line %d: card %d is not %s's" % (path, n, cno, op.code))
        if cno in seen:
            raise BatchError("%s line %d: card %d appears twice" % (path, n, cno))
        seen.add(cno)
        ctype = type_of(f[1])
        if ctype < 0:
            raise BatchError("%s line %d: bad type '%s'" % (path, n, f[1]))
        bal = cents_of(f[2])
        if bal is None or bal < 0 or bal > 99999:
            raise BatchError("%s line %d: bad balance '%s'" % (path, n, f[2]))
        if f[3] != "Y" and f[3] != "N":
            raise BatchError("%s line %d: bad autoload '%s'" % (path, n, f[3]))
        out.append(Card(cno, ctype, bal, f[3] == "Y"))
    return out


def parse_taps(path, op, stations, cards, text):
    body = body_lines(path, text, TAP_HEADER)
    sids = set(s.sid for s in stations)
    cnos = set(c.cno for c in cards)
    out = []
    last = 0
    when = 0
    for k in range(len(body)):
        n = k + 2
        f = fields_of(path, n, body[k], 6)
        tid = small_int(f[0], 1000000, 9999999)
        if tid is None or len(f[0]) != 7:
            raise BatchError("%s line %d: bad tap '%s'" % (path, n, f[0]))
        if tid // 1000000 != op.digit:
            raise BatchError("%s line %d: tap %d is not %s's" % (path, n, tid, op.code))
        if tid <= last:
            raise BatchError("%s line %d: tap %d is out of order" % (path, n, tid))
        last = tid
        cno = small_int(f[1], 0, 99999999)
        if cno is None or cno not in cnos:
            raise BatchError("%s line %d: no card '%s'" % (path, n, f[1]))
        if f[2] not in sids:
            raise BatchError("%s line %d: no station '%s'" % (path, n, f[2]))
        day = small_int(f[3], 1, DAYS)
        if day is None:
            raise BatchError("%s line %d: bad day '%s'" % (path, n, f[3]))
        minute = time_of(f[4])
        if minute is None:
            raise BatchError("%s line %d: bad time '%s'" % (path, n, f[4]))
        if day * 1440 + minute < when:
            raise BatchError("%s line %d: tap %d at day %d %s is before the tap above it" % (path, n, tid, day, f[4]))
        when = day * 1440 + minute
        if f[5] != "IN" and f[5] != "OUT":
            raise BatchError("%s line %d: bad kind '%s'" % (path, n, f[5]))
        out.append(Tap(tid, cno, f[2], day, minute, f[5]))
    return out


# ------------------------------------------------------------ the week

def close_week(op, stations, cards, taps):
    ns = len(stations)
    sidx = {}
    for s in range(ns):
        sidx[stations[s].sid] = s
    cidx = {}
    for k in range(len(cards)):
        cidx[cards[k].cno] = k
    nc = len(cards)
    w = Week()
    w.journeys = []
    w.balance = [c.balance for c in cards]
    w.c_journeys = [0] * nc
    w.c_charged = [0] * nc
    w.c_loads = [0] * nc
    w.c_declined = [0] * nc
    w.c_topups = [0] * nc
    w.day_spent = [0] * (nc * DAYS)
    w.week_spent = [0] * nc
    w.entries = [0] * ns
    w.orphans = 0
    w.peak_charged = 0
    # the open journey by card: station, day, minute, transfer; -1 when none
    o_st = [-1] * nc
    o_day = [0] * nc
    o_min = [0] * nc
    o_xfer = [False] * nc
    # the last tap-out that a transfer may follow: day and minute, -1 when none
    x_day = [-1] * nc
    x_min = [0] * nc

    def incomplete(k):
        c = cards[k]
        fare = op.base + op.per_zone * (op.zones - 1)
        after = with_off(fare, c.ctype)
        w.journeys.append(Journey(c.cno, "INCOMPLETE", stations[o_st[k]].sid, "-", o_day[k], o_min[k], -1, fare,
                                  fare - after, 0, after))
        w.balance[k] -= after
        w.c_journeys[k] += 1
        w.c_charged[k] += after
        o_st[k] = -1
        x_day[k] = -1

    for t in taps:
        k = cidx[t.cno]
        c = cards[k]
        s = sidx[t.sid]
        if t.kind == "IN":
            if o_st[k] >= 0:
                incomplete(k)
            if w.balance[k] < op.base:
                if c.autoload:
                    while w.balance[k] < op.base:
                        w.balance[k] += AUTOLOAD
                        w.c_loads[k] += 1
                else:
                    w.c_declined[k] += 1
                    while w.balance[k] < op.base:
                        w.balance[k] += TOPUP
                        w.c_topups[k] += 1
            o_st[k] = s
            o_day[k] = t.day
            o_min[k] = t.minute
            o_xfer[k] = x_day[k] == t.day and t.minute - x_min[k] <= TRANSFER_MIN
            w.entries[s] += 1
            continue
        if o_st[k] >= 0 and o_day[k] != t.day:
            incomplete(k)
        if o_st[k] < 0:
            w.orphans += 1
            continue
        a = o_st[k]
        if a == s and t.minute - o_min[k] <= CANCEL_MIN:
            w.journeys.append(Journey(c.cno, "CANCEL", stations[a].sid, stations[s].sid, t.day, o_min[k], t.minute,
                                      0, 0, 0, 0))
            o_st[k] = -1
            continue
        zones = abs(stations[a].zone - stations[s].zone)
        fare = op.base + op.per_zone * zones
        at_peak = peak(op, o_day[k], o_min[k])
        if at_peak:
            fare = fare * op.peak // 100
        if o_xfer[k]:
            fare = max(0, fare - op.base)
        after = with_off(fare, c.ctype)
        dcap = with_off(op.daycap, c.ctype)
        wcap = with_off(op.weekcap, c.ctype)
        room = max(0, min(dcap - w.day_spent[k * DAYS + t.day - 1], wcap - w.week_spent[k]))
        charge = min(after, room)
        w.day_spent[k * DAYS + t.day - 1] += charge
        w.week_spent[k] += charge
        w.balance[k] -= charge
        w.c_journeys[k] += 1
        w.c_charged[k] += charge
        if at_peak:
            w.peak_charged += charge
        w.journeys.append(Journey(c.cno, "XFER" if o_xfer[k] else "OK", stations[a].sid, stations[s].sid, t.day,
                                  o_min[k], t.minute, fare, fare - after, after - charge, charge))
        o_st[k] = -1
        if o_xfer[k]:
            x_day[k] = -1
        else:
            x_day[k] = t.day
            x_min[k] = t.minute
    for k in range(nc):
        if o_st[k] >= 0:
            incomplete(k)
    w.taps = len(taps)
    return w


# ------------------------------------------------------------ reports

def ridership_report(op, w):
    kinds = {"OK": 0, "XFER": 0, "CANCEL": 0, "INCOMPLETE": 0}
    by_day = [0] * DAYS
    for j in w.journeys:
        kinds[j.kind] += 1
        if j.kind != "CANCEL":
            by_day[j.day - 1] += 1
    declined = sum(w.c_declined)
    journeys = kinds["OK"] + kinds["XFER"] + kinds["INCOMPLETE"]
    return ("%s ridership: %s taps, %s journeys (%d transfers), %d incomplete, %d cancelled, %d stray tap-outs, "
            "%d declined at the gate; by day %s") % (op.code, group3(w.taps), group3(journeys), kinds["XFER"],
                                         kinds["INCOMPLETE"], kinds["CANCEL"], w.orphans, declined,
                                         " ".join(str(v) for v in by_day))


def revenue_report(op, w):
    fares = 0
    conc = 0
    capped = 0
    charged = 0
    pen = 0
    for j in w.journeys:
        fares += j.fare
        conc += j.conc
        capped += j.capped
        charged += j.charged
        if j.kind == "INCOMPLETE":
            pen += j.charged
    loads = sum(w.c_loads)
    topups = sum(w.c_topups)
    share = w.peak_charged * 1000 // charged if charged > 0 else 0
    return ("%s revenue: %s at full fare, %s concessions, %s capped off, %s charged (%d.%d%% at peak, %s for "
            "incomplete journeys); %d autoloads and %d top-ups at the machine, %s loaded") % (
        op.code, money(fares), money(conc), money(capped), money(charged), share // 10, share % 10, money(pen), loads,
        topups, money(loads * AUTOLOAD + topups * TOPUP))


def cap_report(op, cards, w):
    day_hits = 0
    week_hits = 0
    for k in range(len(cards)):
        dcap = with_off(op.daycap, cards[k].ctype)
        wcap = with_off(op.weekcap, cards[k].ctype)
        for d in range(DAYS):
            if dcap > 0 and w.day_spent[k * DAYS + d] == dcap:
                day_hits += 1
        if wcap > 0 and w.week_spent[k] == wcap:
            week_hits += 1
    order = sorted(range(len(cards)), key=lambda k: (-w.c_journeys[k], -w.c_charged[k], cards[k].cno))
    top = []
    for k in order[:3]:
        top.append("%d %s %d journeys %s" % (cards[k].cno, TYPES[cards[k].ctype], w.c_journeys[k],
                                             money(w.c_charged[k])))
    return "%s caps: %d card-days at the day cap, %d cards at the week cap; most riding %s" % (
        op.code, day_hits, week_hits, ", ".join(top))


def station_report(op, stations, w):
    order = sorted(range(len(stations)), key=lambda s: (-w.entries[s], stations[s].sid))
    busy = []
    for s in order[:3]:
        busy.append("%s %s %d" % (stations[s].sid, stations[s].name, w.entries[s]))
    sidx = {}
    for s in range(len(stations)):
        sidx[stations[s].sid] = s
    crossed = [0] * op.zones
    for j in w.journeys:
        if j.kind == "OK" or j.kind == "XFER":
            crossed[abs(stations[sidx[j.frm]].zone - stations[sidx[j.to]].zone)] += 1
    cells = ["%d:%d" % (z, crossed[z]) for z in range(op.zones)]
    return "%s stations: busiest %s; journeys by zones crossed %s" % (op.code, ", ".join(busy), " ".join(cells))


def balance_report(op, cards, w):
    total = 0
    neg = 0
    low = 0
    for k in range(len(cards)):
        total += w.balance[k]
        if w.balance[k] < 0:
            neg += 1
        if w.balance[k] < w.balance[low]:
            low = k
    cells = []
    for t in range(4):
        n = 0
        spent = 0
        for k in range(len(cards)):
            if cards[k].ctype == t:
                n += 1
                spent += w.c_charged[k]
        avg = spent // n if n > 0 else 0
        cells.append("%s %d at %s" % (TYPES[t], n, money(avg)))
    return "%s balances: %s held, %d cards below zero, lowest %d at %s; average spend %s" % (
        op.code, money(total), neg, cards[low].cno, money(w.balance[low]), ", ".join(cells))


# ------------------------------------------------------------ the fund

# An operator claims what its concessions cost it; the fund repays what it
# has left.
def claim(fund, op, w):
    asked = 0
    for j in w.journeys:
        asked += j.conc
    granted = min(asked, fund.left)
    fund.left -= granted
    fund.log.append("%s,%s,%s,%s" % (op.code, dollars(asked), dollars(granted), dollars(fund.left)))
    return "%s subsidy: claimed %s, repaid %s, %s unpaid; %s left in the fund" % (
        op.code, money(asked), money(granted), money(asked - granted), money(fund.left))


# ------------------------------------------------------------ the job

def run():
    ops = [
        Operator("MTR", 1, 7101, 16, 4, 250, 60, 125, 900, 3600),
        Operator("BUS", 2, 7202, 20, 3, 200, 0, 100, 600, 2400),
        Operator("LRT", 3, 7303, 12, 3, 225, 40, 115, 800, 3200),
        Operator("FRY", 4, 7404, 6, 2, 400, 150, 100, 1100, 4400),
    ]
    sts = [build_stations(o) for o in ops]
    cds = [build_cards(o) for o in ops]
    tps = [build_taps(ops[i], sts[i], cds[i]) for i in range(4)]
    names = []
    for o in ops:
        names += [o.code + "_stations.csv", o.code + "_cards.csv", o.code + "_taps.csv"]
    for i in range(4):
        write_text(names[i * 3], encode_stations(sts[i]))
        write_text(names[i * 3 + 1], encode_cards(cds[i]))
        write_text(names[i * 3 + 2], encode_taps(tps[i]))
    whole = read_text("MTR_taps.csv")
    write_text("MTR_taps.bad", damage(whole, 50, 2, "MTR-99"))
    whole2 = read_text("LRT_cards.csv")
    write_text("LRT_cards.bad", damage(whole2, 20, 2, "12.5"))
    texts = [read_text(p) for p in names]
    labels = []
    for o in ops:
        labels += [o.code + " stations", o.code + " cards", o.code + " taps"]
    for k in range(12):
        print(file_line(labels[k], texts[k]))
    ps = [parse_stations(names[i * 3], ops[i], texts[i * 3]) for i in range(4)]
    pc = [parse_cards(names[i * 3 + 1], ops[i], texts[i * 3 + 1]) for i in range(4)]
    pt = [parse_taps(names[i * 3 + 2], ops[i], ps[i], pc[i], texts[i * 3 + 2]) for i in range(4)]
    try:
        parse_taps("MTR_taps.bad", ops[0], ps[0], pc[0], read_text("MTR_taps.bad"))
        print("MTR_taps.bad: parsed")
    except BatchError as e:
        print(e)
    try:
        parse_cards("LRT_cards.bad", ops[2], read_text("LRT_cards.bad"))
        print("LRT_cards.bad: parsed")
    except BatchError as e:
        print(e)
    try:
        parse_stations("FRY_stations.csv", ops[1], texts[9])
        print("FRY_stations.csv: parsed")
    except BatchError as e:
        print(e)
    same = all([a.key() for a in ps[i]] == [b.key() for b in sts[i]] and
               [a.key() for a in pc[i]] == [b.key() for b in cds[i]] and
               [a.key() for a in pt[i]] == [b.key() for b in tps[i]] for i in range(4))
    print("parsed files match the built ones: %s" % ("true" if same else "false"))
    weeks = [close_week(ops[i], ps[i], pc[i], pt[i]) for i in range(4)]
    for i in range(4):
        write_text(ops[i].code + "_journeys.csv", encode_journeys(weeks[i]))
    for i in range(4):
        write_text(ops[i].code + "_statements.csv", encode_statements(pc[i], weeks[i]))
    for i in range(4):
        print(file_line(ops[i].code + " journeys", read_text(ops[i].code + "_journeys.csv")))
        print(file_line(ops[i].code + " statements", read_text(ops[i].code + "_statements.csv")))
    for i in range(4):
        print(ridership_report(ops[i], weeks[i]))
    for i in range(4):
        print(revenue_report(ops[i], weeks[i]))
    for i in range(4):
        print(cap_report(ops[i], pc[i], weeks[i]))
    for i in range(4):
        print(station_report(ops[i], ps[i], weeks[i]))
    for i in range(4):
        print(balance_report(ops[i], pc[i], weeks[i]))
    fund = Fund()
    seq = [0, 1, 2, 3]
    if os.environ.get("FARE_REVERSE") == "1":
        seq = [3, 2, 1, 0]
    for i in seq:
        print(claim(fund, ops[i], weeks[i]))
    write_text("fund.log", "\n".join(fund.log) + "\n")
    back = read_text("fund.log")
    rows = len(back.split("\n")) - 1
    print("fund.log: %s entries, %s bytes, fnv %08x" % (group3(rows), group3(len(back)), text_sum(back)))
    for p in names + ["MTR_taps.bad", "LRT_cards.bad", "fund.log"] + \
            [o.code + "_journeys.csv" for o in ops] + [o.code + "_statements.csv" for o in ops]:
        os.remove(p)


def main():
    try:
        run()
    except BatchError as e:
        print("fare_cap failed: %s" % e)


main()
