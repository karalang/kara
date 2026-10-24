"""pipeline/toll_clearing, the Python mirror: a toll clearinghouse's
October batch over four toll agencies (the Harbor bridge, the Ridge
turnpike, the Lakeshore express lanes and the Valley tunnel), from the
account file, the stolen-tag hotlist and each agency's transaction file
to the rating that identifies each vehicle and prices its trip, the
per-agency reports, and one ledger of prepaid account balances that every
agency's tolls draw on. It wrote expected.out.

Run from this directory: python3 -I -B toll_clearing.py > expected.out
TOLL_REVERSE=1 posts the agencies in reverse order."""
import os

MASK32 = 4294967295
# Day 0 is Thursday 1 October 2026.
FIRST_WEEKDAY = 3
WEEKDAYS = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"]
MONTH_START = [0, 31, 59, 90, 120, 151, 181, 212, 243, 273, 304, 334]
MONTH_DAYS = [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
DAY0 = 273
MONTH_LEN = 31
ACCOUNTS = 300
STATES = ["NJ", "NY", "PA", "DE"]
STRANGER_STATES = ["MD", "CT", "VA", "OH"]
REPLENISH = [2500, 3500, 5000]
CARDS = [0, 2, 3, 4, 4, 6, 6, 8]
# The Ridge turnpike's interchanges, by milepost.
MILEPOSTS = [0, 6, 11, 19, 27, 34, 42, 51, 58, 66, 73, 81, 90, 98, 105, 113, 120, 128]
# Cents a mile on the Ridge, by vehicle class (axles).
PER_MILE = [0, 0, 6, 9, 13, 18, 24]
CODES = ["S", "C", "D", "U", "V"]
CODE_NAMES = ["stolen tag", "account closed", "duplicate read", "plate unreadable", "plate not on file"]
BANDS = [["peak", "off-peak"], ["short", "medium", "long", "no entry"],
         ["light", "busy", "jammed", "weekend"], ["day", "night"]]
NOTICE_FEE = 300
NOTICES_TO_SUSPEND = 3
WEEKDAYS_IN_MONTH = 22
WEEKEND_DAYS_IN_MONTH = 9


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


def plural(n, word):
    return f"{n} {word}" if n == 1 else f"{n} {word}s"


def is_digits(t):
    return len(t) > 0 and all("0" <= ch <= "9" for ch in t)


def is_upper(t):
    return len(t) > 0 and all("A" <= ch <= "Z" for ch in t)


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


def day_name(day):
    return f"{WEEKDAYS[(FIRST_WEEKDAY + day) % 7]} {date_of(day)[5:]}"


def weekday(t):
    return (FIRST_WEEKDAY + t // 86400) % 7


def hour_of(t):
    return t % 86400 // 3600


def stamp(t):
    s = t % 86400
    return f"{date_of(t // 86400)} {s // 3600:02}:{s % 3600 // 60:02}:{s % 60:02}"


# Seconds into October of a 'YYYY-MM-DD HH:MM:SS' stamp, or None.
def time_of(t):
    if len(t) != 19 or t[10] != " " or t[13] != ":" or t[16] != ":":
        return None
    day = day_of(t[0:10])
    if day is None or day < 0 or day >= MONTH_LEN:
        return None
    h, m, s = t[11:13], t[14:16], t[17:19]
    if not (is_digits(h) and is_digits(m) and is_digits(s)):
        return None
    h, m, s = int(h), int(m), int(s)
    if h > 23 or m > 59 or s > 59:
        return None
    return day * 86400 + h * 3600 + m * 60 + s


# The Luhn check digit of a string of digits.
def luhn_digit(payload):
    total = 0
    k = 0
    for ch in reversed(payload):
        d = ord(ch) - 48
        if k % 2 == 0:
            d *= 2
            if d > 9:
                d -= 9
        total += d
        k += 1
    return (10 - total % 10) % 10


def tag_ok(t):
    return len(t) == 10 and is_digits(t) and luhn_digit(t[:9]) == ord(t[9]) - 48


def plate_ok(t):
    return len(t) == 9 and is_upper(t[0:5]) and is_digits(t[5:9])


def letters3(h):
    return chr(65 + h % 26) + chr(65 + h // 26 % 26) + chr(65 + h // 676 % 26)


# ------------------------------------------------------------ the data

class Account:
    def __init__(self, acct, tag, plate, cls, balance, threshold, replenish, cards, closed):
        self.acct = acct
        self.tag = tag
        self.plate = plate
        self.cls = cls
        self.balance = balance
        self.threshold = threshold
        self.replenish = replenish
        self.cards = cards
        self.closed = closed

    def key(self):
        return (self.acct, self.tag, self.plate, self.cls, self.balance, self.threshold,
                self.replenish, self.cards, self.closed)


class Hot:
    def __init__(self, tag, reported):
        self.tag = tag
        self.reported = reported

    def key(self):
        return (self.tag, self.reported)


class Txn:
    def __init__(self, txn, time, lane, tag, plate, conf, cls, entry, entry_time):
        self.txn = txn
        self.time = time
        self.lane = lane
        self.tag = tag
        self.plate = plate
        self.conf = conf
        self.cls = cls
        self.entry = entry
        self.entry_time = entry_time

    def key(self):
        return (self.txn, self.time, self.lane, self.tag, self.plate, self.conf, self.cls,
                self.entry, self.entry_time)


class Agency:
    def __init__(self, name, code, seed, kind, lanes, panel, strangers):
        self.name = name
        self.code = code
        self.seed = seed
        # 0 bridge, 1 turnpike, 2 express lanes, 3 tunnel
        self.kind = kind
        self.lanes = lanes
        self.panel = panel
        self.strangers = strangers


class Outcome:
    def __init__(self, code, acct, how, cls, base, toll, band, extra):
        self.code = code
        self.acct = acct
        self.how = how
        self.cls = cls
        self.base = base
        self.toll = toll
        self.band = band
        self.extra = extra


class Ledger:
    def __init__(self, accounts):
        self.bal = [a.balance for a in accounts]
        self.used = [0 for _ in accounts]
        self.notices = [0 for _ in accounts]
        self.susp = [False for _ in accounts]
        self.agencies = [0 for _ in accounts]
        self.suspended = 0
        self.debited = 0
        self.replenished = 0
        self.notice_n = 0
        self.billed = 0
        self.log = []


# ------------------------------------------------------------ building

def build_accounts():
    out = []
    for i in range(ACCOUNTS):
        base9 = 100000000 + i * 2999999 + mix(7, i, 2) % 2999999
        tag = f"{base9}{luhn_digit(str(base9))}"
        plate = f"{STATES[mix(7, i, 3) % 4]}{letters3(mix(7, i, 4))}{1000 + i * 7919 % 9000:04}"
        r = mix(7, i, 5) % 100
        cls = 2 if r < 85 else 3 if r < 92 else 4 if r < 96 else 5 if r < 99 else 6
        balance = 1500 + mix(7, i, 6) % 36 * 100
        replenish = REPLENISH[mix(7, i, 7) % 3]
        cards = CARDS[mix(7, i, 8) % 8]
        closed = -1
        if mix(7, i, 9) % 40 == 0:
            closed = 2 + mix(7, i, 10) % 28
        out.append(Account(f"A{20001 + i}", tag, plate, cls, balance, 1000, replenish, cards, closed))
    return out


def build_hotlist(accounts):
    out = []
    for i in range(ACCOUNTS):
        if mix(7, i, 11) % 60 == 0:
            day = 3 + mix(7, i, 12) % 25
            out.append(Hot(accounts[i].tag, day * 86400 + (9 + mix(7, i, 13) % 9) * 3600 + mix(7, i, 14) % 3600))
    return out


def build_txns(ag, accounts):
    events = []
    seq = 0
    s = ag.seed
    for i in range(ACCOUNTS):
        if mix(s, i, 20) % 100 >= ag.panel:
            continue
        a = accounts[i]
        home = 1 + mix(s, i, 27) % 18
        work = 1 + mix(s, i, 28) % 18
        if work == home:
            work = home % 18 + 1
        for d in range(MONTH_LEN):
            wd = (FIRST_WEEKDAY + d) % 7
            key0 = i * 31 + d
            r = mix(s, key0, 21) % 100
            if wd < 5:
                n = 2 if r < 20 else 1 if r < 42 else 0
            else:
                n = 1 if r < 18 else 0
            for t in range(n):
                key = key0 * 2 + t
                if n == 2:
                    hour = (6 if t == 0 else 16) + mix(s, key, 24) % 4
                else:
                    hour = mix(s, key, 24) % 24
                time = d * 86400 + hour * 3600 + mix(s, key, 22) % 3600
                entry = 0
                if ag.kind == 1:
                    if n == 2 and t == 0:
                        entry, lane = home, work
                    elif n == 2:
                        entry, lane = work, home
                    else:
                        entry = home
                        lane = 1 + mix(s, key, 26) % 18
                        if lane == entry:
                            lane = entry % 18 + 1
                elif ag.kind == 2:
                    dirn = 0 if n == 2 and t == 0 else 1 if n == 2 else mix(s, key, 26) % 2
                    lane = 1 + 2 * dirn + mix(s, key, 25) % 2
                else:
                    lane = 1 + mix(s, key, 25) % ag.lanes
                tag = a.tag if mix(s, key, 30) % 12 != 0 else ""
                q = mix(s, key, 31)
                conf = q // 4 % 80 if q % 4 == 0 else 80 + q // 4 % 20
                plate = a.plate
                if conf < 20:
                    plate = ""
                    conf = 0
                c = mix(s, key, 33) % 50
                cls = min(6, a.cls + 1) if c < 2 else max(2, a.cls - 1) if c == 2 else a.cls
                entry_time = -1
                if ag.kind == 1 and tag != "":
                    miles = abs(MILEPOSTS[lane - 1] - MILEPOSTS[entry - 1])
                    entry_time = time - (miles * 60 + 120 + mix(s, key, 34) % 600)
                else:
                    entry = 0
                events.append((time, seq, lane, tag, plate, conf, cls, entry, entry_time))
                seq += 1
                if mix(s, key, 35) % 30 == 0:
                    events.append((time + 20 + mix(s, key, 36) % 70, seq, lane, tag, plate, conf, cls, entry, entry_time))
                    seq += 1
    for k in range(ag.strangers):
        d = mix(s, k, 40) % MONTH_LEN
        time = d * 86400 + mix(s, k, 41) % 24 * 3600 + mix(s, k, 42) % 3600
        h = mix(s, k, 44)
        plate = f"{STRANGER_STATES[mix(s, k, 43) % 4]}{letters3(h)}{1000 + h // 17576 % 9000:04}"
        q = mix(s, k, 45)
        conf = q // 4 % 80 if q % 4 == 0 else 80 + q // 4 % 20
        if conf < 20:
            plate = ""
            conf = 0
        cls = 2 if mix(s, k, 46) % 10 < 8 else 3 + mix(s, k, 47) % 3
        lane = 1 + mix(s, k, 48) % ag.lanes
        events.append((time, seq, lane, "", plate, conf, cls, 0, -1))
        seq += 1
    events.sort(key=lambda e: (e[0], e[1]))
    out = []
    for k, e in enumerate(events):
        out.append(Txn(f"{ag.code}{k + 1:07}", e[0], e[2], e[3], e[4], e[5], e[6], e[7], e[8]))
    return out


# ------------------------------------------------------------ files

ACCOUNTS_HEADER = "account,tag,plate,class,balance,threshold,replenish,cards,closed"
HOTLIST_HEADER = "tag,reported"
TXNS_HEADER = "txn,time,lane,tag,plate,conf,class,entry,entry_time"


def encode_accounts(accounts):
    out = ACCOUNTS_HEADER + "\n"
    for a in accounts:
        closed = date_of(a.closed) if a.closed >= 0 else ""
        out += (f"{a.acct},{a.tag},{a.plate},{a.cls},{dollars(a.balance)},{dollars(a.threshold)},"
                f"{dollars(a.replenish)},{a.cards},{closed}\n")
    return out


def encode_hotlist(hot):
    out = HOTLIST_HEADER + "\n"
    for h in hot:
        out += f"{h.tag},{stamp(h.reported)}\n"
    return out


def encode_txns(txns):
    out = TXNS_HEADER + "\n"
    for x in txns:
        entry = f"{x.entry:02}" if x.entry > 0 else ""
        et = stamp(x.entry_time) if x.entry_time >= 0 else ""
        out += f"{x.txn},{stamp(x.time)},{x.lane:02},{x.tag},{x.plate},{x.conf},{x.cls},{entry},{et}\n"
    return out


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


# The first line from n on whose field is not empty.
def first_with(text, n, field):
    lines = text.split("\n")
    while lines[n].split(",")[field] == "":
        n += 1
    return n


# ------------------------------------------------------------ parsing

def body_lines(path, text, header):
    lines = text.split("\n")
    if len(lines) < 2 or lines[-1] != "":
        raise BatchError(f"{path}: no final newline")
    if lines[0] != header:
        raise BatchError(f"{path} line 1: bad header")
    return lines[1:-1]


def money_field(path, n, what, t):
    v = cents_of(t)
    if v is None:
        raise BatchError(f"{path} line {n}: bad {what} '{t}'")
    return v


def int_field(path, n, what, t, lo, hi):
    v = small_int(t, lo, hi)
    if v is None:
        raise BatchError(f"{path} line {n}: bad {what} '{t}'")
    return v


def time_field(path, n, what, t):
    v = time_of(t)
    if v is None:
        raise BatchError(f"{path} line {n}: bad {what} '{t}'")
    return v


def parse_accounts(path, text):
    body = body_lines(path, text, ACCOUNTS_HEADER)
    out = []
    tags = set()
    plates = set()
    for k, line in enumerate(body):
        n = k + 2
        f = line.split(",")
        if len(f) != 9:
            raise BatchError(f"{path} line {n}: {len(f)} fields, not 9")
        if len(f[0]) != 6 or f[0][0] != "A" or not is_digits(f[0][1:]):
            raise BatchError(f"{path} line {n}: bad account '{f[0]}'")
        if not tag_ok(f[1]):
            raise BatchError(f"{path} line {n}: bad tag '{f[1]}'")
        if f[1] in tags:
            raise BatchError(f"{path} line {n}: tag {f[1]} is on two accounts")
        if not plate_ok(f[2]) or f[2][0:2] not in STATES:
            raise BatchError(f"{path} line {n}: bad plate '{f[2]}'")
        if f[2] in plates:
            raise BatchError(f"{path} line {n}: plate {f[2]} is on two accounts")
        tags.add(f[1])
        plates.add(f[2])
        cls = int_field(path, n, "class", f[3], 2, 6)
        balance = money_field(path, n, "balance", f[4])
        threshold = money_field(path, n, "threshold", f[5])
        replenish = money_field(path, n, "replenish amount", f[6])
        cards = int_field(path, n, "card limit", f[7], 0, 9)
        closed = -1
        if f[8] != "":
            d = day_of(f[8])
            if d is None or d < 0 or d >= MONTH_LEN:
                raise BatchError(f"{path} line {n}: bad closing date '{f[8]}'")
            closed = d
        out.append(Account(f[0], f[1], f[2], cls, balance, threshold, replenish, cards, closed))
    return out


def parse_hotlist(path, text, accounts):
    body = body_lines(path, text, HOTLIST_HEADER)
    known = set(a.tag for a in accounts)
    out = []
    for k, line in enumerate(body):
        n = k + 2
        f = line.split(",")
        if len(f) != 2:
            raise BatchError(f"{path} line {n}: {len(f)} fields, not 2")
        if not tag_ok(f[0]):
            raise BatchError(f"{path} line {n}: bad tag '{f[0]}'")
        if f[0] not in known:
            raise BatchError(f"{path} line {n}: tag {f[0]} is on no account")
        out.append(Hot(f[0], time_field(path, n, "report time", f[1])))
    return out


def parse_txns(path, ag, text):
    body = body_lines(path, text, TXNS_HEADER)
    out = []
    last = 0
    for k, line in enumerate(body):
        n = k + 2
        f = line.split(",")
        if len(f) != 9:
            raise BatchError(f"{path} line {n}: {len(f)} fields, not 9")
        if len(f[0]) != 8 or f[0][0] != ag.code or not is_digits(f[0][1:]):
            raise BatchError(f"{path} line {n}: bad transaction '{f[0]}' for {ag.name}")
        time = time_field(path, n, "time", f[1])
        if time < last:
            raise BatchError(f"{path} line {n}: {f[1]} is earlier than the line before")
        last = time
        lane = int_field(path, n, "lane", f[2], 1, ag.lanes)
        if f[3] != "" and not tag_ok(f[3]):
            raise BatchError(f"{path} line {n}: bad tag '{f[3]}'")
        if f[4] != "" and not plate_ok(f[4]):
            raise BatchError(f"{path} line {n}: bad plate '{f[4]}'")
        conf = int_field(path, n, "confidence", f[5], 0, 99)
        cls = int_field(path, n, "class", f[6], 2, 6)
        entry = 0
        entry_time = -1
        if (f[7] == "") != (f[8] == ""):
            raise BatchError(f"{path} line {n}: an entry needs both its interchange and its time")
        if f[7] != "":
            if ag.kind != 1:
                raise BatchError(f"{path} line {n}: an entry on a {ag.name} lane")
            entry = int_field(path, n, "entry interchange", f[7], 1, 18)
            entry_time = time_field(path, n, "entry time", f[8])
            if entry_time >= time:
                raise BatchError(f"{path} line {n}: entry at {f[8]} is not before the exit")
        out.append(Txn(f[0], time, lane, f[3], f[4], conf, cls, entry, entry_time))
    return out


# ------------------------------------------------------------ rating

def slot_key(t, lane):
    return t // 86400 * 1000 + hour_of(t) * 10 + (lane - 1) // 2


def toll_for(ag, x, cls, counts):
    h = hour_of(x.time)
    if ag.kind == 0:
        peak = weekday(x.time) < 5 and (6 <= h < 10 or 16 <= h < 20)
        if peak:
            return 600 + 300 * (cls - 2), "peak"
        return 450 + 225 * (cls - 2), "off-peak"
    if ag.kind == 1:
        out = MILEPOSTS[x.lane - 1]
        if x.entry == 0:
            miles = max(out - MILEPOSTS[0], MILEPOSTS[17] - out)
            band = "no entry"
        else:
            miles = abs(out - MILEPOSTS[x.entry - 1])
            band = "short" if miles < 30 else "medium" if miles < 80 else "long"
        return max(75, (miles * PER_MILE[cls] + 4) // 5 * 5), band
    if ag.kind == 2:
        if weekday(x.time) >= 5:
            return 150 * (cls - 1), "weekend"
        n = counts.get(slot_key(x.time, x.lane), 0)
        if n <= 3:
            return 150 * (cls - 1), "light"
        if n <= 6:
            return 300 * (cls - 1), "busy"
        return 500 * (cls - 1), "jammed"
    if h >= 22 or h < 6:
        return 250 + 125 * (cls - 2), "night"
    return 400 + 200 * (cls - 2), "day"


def rate(ag, accounts, hot, txns):
    by_tag = {}
    by_plate = {}
    for i, a in enumerate(accounts):
        by_tag[a.tag] = i
        by_plate[a.plate] = i
    stolen = {}
    for h in hot:
        stolen[h.tag] = h.reported
    counts = {}
    if ag.kind == 2:
        for x in txns:
            k = slot_key(x.time, x.lane)
            counts[k] = counts.get(k, 0) + 1
    last = [-1 for _ in accounts]
    out = []
    for x in txns:
        idx = -1
        how = ""
        if x.tag != "":
            if x.tag in stolen and x.time >= stolen[x.tag]:
                out.append(Outcome("S", -1, "", 0, 0, 0, "", 0))
                continue
            idx = by_tag.get(x.tag, -1)
            how = "tag"
        if idx < 0:
            if x.plate == "" or x.conf < 80:
                out.append(Outcome("U", -1, "", 0, 0, 0, "", 0))
                continue
            idx = by_plate.get(x.plate, -1)
            if idx < 0:
                out.append(Outcome("V", -1, "", 0, 0, 0, "", 0))
                continue
            how = "plate"
        a = accounts[idx]
        if a.closed >= 0 and x.time // 86400 >= a.closed:
            out.append(Outcome("C", idx, how, 0, 0, 0, "", 0))
            continue
        if last[idx] >= 0 and x.time - last[idx] < 120:
            out.append(Outcome("D", idx, how, 0, 0, 0, "", 0))
            continue
        cls = max(x.cls, a.cls)
        base, band = toll_for(ag, x, cls, counts)
        at_acct, _ = toll_for(ag, x, a.cls, counts)
        toll = base + base // 2 if how == "plate" else base
        last[idx] = x.time
        out.append(Outcome("R", idx, how, cls, base, toll, band, base - at_acct))
    return out


# ------------------------------------------------------------ reports

def txn_report(ag, txns, outs):
    rated = sum(1 for o in outs if o.code == "R")
    by_tag = sum(1 for o in outs if o.code == "R" and o.how == "tag")
    lines = []
    counts = []
    for c in CODES:
        n = sum(1 for o in outs if o.code == c)
        if n > 0:
            counts.append(f"{c} {n}")
    head = (f"{ag.name}: {group3(len(txns))} transactions, {group3(rated)} rated ({group3(by_tag)} by tag, "
            f"{group3(rated - by_tag)} by plate), {group3(len(txns) - rated)} not billed")
    if counts:
        head += ": " + ", ".join(counts)
    lines.append(head)
    for ci, c in enumerate(CODES):
        for x, o in zip(txns, outs):
            if o.code == c:
                seen = x.tag if x.tag != "" else x.plate if x.plate != "" else "no read"
                lines.append(f"  {c} {CODE_NAMES[ci]}: {x.txn} {stamp(x.time)} lane {x.lane} {seen}")
                break
    return lines


def revenue_report(ag, outs):
    total = 0
    rated = 0
    sur = 0
    over = 0
    extra = 0
    parts = []
    for o in outs:
        if o.code == "R":
            total += o.toll
            rated += 1
            sur += o.toll - o.base
            if o.extra > 0:
                over += 1
                extra += o.extra
    for band in BANDS[ag.kind]:
        s = sum(o.toll for o in outs if o.code == "R" and o.band == band)
        n = sum(1 for o in outs if o.code == "R" and o.band == band)
        parts.append(f"{band} {money(s)} on {group3(n)}")
    return (f"{ag.name} revenue: {money(total)} on {group3(rated)} trips; {', '.join(parts)}; "
            f"plate surcharges {money(sur)}; {over} trips over the account class, {money(extra)} more")


def traffic_report(ag, txns):
    per_day = [0] * MONTH_LEN
    per_hour = [0] * 24
    per_lane = [0] * (ag.lanes + 1)
    for x in txns:
        per_day[x.time // 86400] += 1
        per_hour[hour_of(x.time)] += 1
        per_lane[x.lane] += 1
    bd = 0
    for d in range(MONTH_LEN):
        if per_day[d] > per_day[bd]:
            bd = d
    bh = 0
    for h in range(24):
        if per_hour[h] > per_hour[bh]:
            bh = h
    bl = 1
    for lane in range(1, ag.lanes + 1):
        if per_lane[lane] > per_lane[bl]:
            bl = lane
    wk = 0
    we = 0
    for d in range(MONTH_LEN):
        if (FIRST_WEEKDAY + d) % 7 < 5:
            wk += per_day[d]
        else:
            we += per_day[d]
    return (f"{ag.name} traffic: busiest {day_name(bd)} with {per_day[bd]}, busiest hour {bh:02}:00 with "
            f"{per_hour[bh]} over the month, busiest lane {bl} with {per_lane[bl]}; "
            f"{tenths(wk, WEEKDAYS_IN_MONTH)} a weekday, {tenths(we, WEEKEND_DAYS_IN_MONTH)} a weekend day")


def accounts_report(ag, accounts, outs):
    tolls = {}
    trips = {}
    plated = set()
    for o in outs:
        if o.code == "R":
            tolls[o.acct] = tolls.get(o.acct, 0) + o.toll
            trips[o.acct] = trips.get(o.acct, 0) + 1
            if o.how == "plate":
                plated.add(o.acct)
    ranked = sorted(tolls, key=lambda i: (-tolls[i], accounts[i].acct))
    top = ", ".join(f"{accounts[i].acct} {money(tolls[i])} on {trips[i]}" for i in ranked[:3])
    return f"{ag.name} accounts: {len(tolls)} billed, {len(plated)} with a trip billed by plate; top {top}"


# ------------------------------------------------------------ posting

def post_tolls(led, ag, accounts, txns, outs):
    trips = 0
    debited = 0
    rep = 0
    rep_amt = 0
    notices = 0
    billed = 0
    susp = []
    seen = set()
    for x, o in zip(txns, outs):
        if o.code != "R":
            continue
        i = o.acct
        a = accounts[i]
        trips += 1
        if i not in seen:
            seen.add(i)
            led.agencies[i] += 1
        if not led.susp[i]:
            if led.bal[i] - o.toll < a.threshold and led.used[i] < a.cards:
                led.bal[i] += a.replenish
                led.used[i] += 1
                rep += 1
                rep_amt += a.replenish
                led.log.append(f"{stamp(x.time)} {x.txn} {a.acct} replenish {dollars(a.replenish)} {dollars(led.bal[i])}")
            if led.bal[i] >= o.toll:
                led.bal[i] -= o.toll
                debited += o.toll
                led.log.append(f"{stamp(x.time)} {x.txn} {a.acct} debit {dollars(o.toll)} {dollars(led.bal[i])}")
                continue
        bill = o.base * 3 // 2 + NOTICE_FEE
        led.notices[i] += 1
        notices += 1
        billed += bill
        led.log.append(f"{stamp(x.time)} {x.txn} {a.acct} notice {dollars(bill)} {dollars(led.bal[i])}")
        if led.notices[i] == NOTICES_TO_SUSPEND:
            led.susp[i] = True
            led.suspended += 1
            susp.append(f"{a.acct} {stamp(x.time)[5:16]}")
    led.debited += debited
    led.replenished += rep_amt
    led.notice_n += notices
    led.billed += billed
    out = (f"{ag.name} posted: {group3(trips)} trips, {money(debited)} from prepaid balances, "
           f"{plural(rep, 'replenishment')} for {money(rep_amt)}, {group3(notices)} notices billing "
           f"{money(billed)}; {plural(len(susp), 'account')} suspended")
    if susp:
        out += "\n  suspended after " + str(NOTICES_TO_SUSPEND) + " notices: " + ", ".join(susp)
    return out


def ledger_summary(led, accounts):
    out = []
    used_up = sum(1 for i, a in enumerate(accounts) if a.cards > 0 and led.used[i] == a.cards)
    active = [i for i in range(len(accounts)) if led.agencies[i] > 0]
    multi = sum(1 for i in active if led.agencies[i] > 1)
    below = sum(1 for i in active if led.bal[i] < accounts[i].threshold)
    high = sorted(active, key=lambda i: (-led.bal[i], accounts[i].acct))[:3]
    out.append(f"month: {money(led.debited)} debited, {money(led.replenished)} replenished, "
               f"{group3(led.notice_n)} notices billing {money(led.billed)}; {used_up} accounts used every "
               f"replenishment their card allows, {led.suspended} suspended")
    out.append(f"accounts: {len(active)} with trips, {multi} at more than one agency, {below} closing below "
               f"their threshold; largest balances "
               + ", ".join(f"{accounts[i].acct} {money(led.bal[i])}" for i in high))
    return out


def file_line(name, text):
    rows = len(text.split("\n")) - 2
    return f"{name}: {group3(rows)} rows, {group3(len(text))} bytes, fnv {text_sum(text):08x}"


# ------------------------------------------------------------ the job

def run():
    harbor = Agency("harbor", "H", 101, 0, 8, 30, 110)
    ridge = Agency("ridge", "R", 202, 1, 18, 26, 90)
    lakeshore = Agency("lakeshore", "L", 303, 2, 4, 22, 70)
    valley = Agency("valley", "V", 404, 3, 4, 28, 100)

    accounts = build_accounts()
    hot = build_hotlist(accounts)
    x1 = build_txns(harbor, accounts)
    x2 = build_txns(ridge, accounts)
    x3 = build_txns(lakeshore, accounts)
    x4 = build_txns(valley, accounts)

    write_text("accounts.csv", encode_accounts(accounts))
    write_text("hotlist.csv", encode_hotlist(hot))
    write_text("harbor_txns.csv", encode_txns(x1))
    write_text("ridge_txns.csv", encode_txns(x2))
    write_text("lakeshore_txns.csv", encode_txns(x3))
    write_text("valley_txns.csv", encode_txns(x4))

    # A copy of Valley's file with one tag's check digit off by one, and a
    # copy of Ridge's with one entry time after its exit.
    whole = read_text("valley_txns.csv")
    n = first_with(whole, 40, 3)
    tag = whole.split("\n")[n].split(",")[3]
    write_text("valley_txns.bad", damage(whole, n, 3, tag[:9] + str((int(tag[9]) + 1) % 10)))
    whole = read_text("ridge_txns.csv")
    n = first_with(whole, 60, 7)
    exit_at = whole.split("\n")[n].split(",")[1]
    write_text("ridge_txns.bad", damage(whole, n, 8, exit_at[:17] + "59"))

    ta = read_text("accounts.csv")
    th = read_text("hotlist.csv")
    t1 = read_text("harbor_txns.csv")
    t2 = read_text("ridge_txns.csv")
    t3 = read_text("lakeshore_txns.csv")
    t4 = read_text("valley_txns.csv")
    print(file_line("accounts", ta))
    print(file_line("hotlist", th))
    print(file_line("harbor", t1))
    print(file_line("ridge", t2))
    print(file_line("lakeshore", t3))
    print(file_line("valley", t4))

    ga = parse_accounts("accounts.csv", ta)
    gh = parse_hotlist("hotlist.csv", th, ga)
    g1 = parse_txns("harbor_txns.csv", harbor, t1)
    g2 = parse_txns("ridge_txns.csv", ridge, t2)
    g3 = parse_txns("lakeshore_txns.csv", lakeshore, t3)
    g4 = parse_txns("valley_txns.csv", valley, t4)
    for path, ag in [("valley_txns.bad", valley), ("ridge_txns.bad", ridge)]:
        try:
            parse_txns(path, ag, read_text(path))
            print(f"{path}: parsed")
        except BatchError as e:
            print(e)
    try:
        parse_txns("harbor_txns.csv", valley, t1)
        print("harbor_txns.csv: parsed")
    except BatchError as e:
        print(e)
    same = ([a.key() for a in ga] == [a.key() for a in accounts] and [h.key() for h in gh] == [h.key() for h in hot]
            and all([x.key() for x in g] == [x.key() for x in b] for g, b in [(g1, x1), (g2, x2), (g3, x3), (g4, x4)]))
    print(f"parsed files match the built ones: {'true' if same else 'false'}")

    o1 = rate(harbor, ga, gh, g1)
    o2 = rate(ridge, ga, gh, g2)
    o3 = rate(lakeshore, ga, gh, g3)
    o4 = rate(valley, ga, gh, g4)

    for ag, g, o in [(harbor, g1, o1), (ridge, g2, o2), (lakeshore, g3, o3), (valley, g4, o4)]:
        for line in txn_report(ag, g, o):
            print(line)
    for ag, o in [(harbor, o1), (ridge, o2), (lakeshore, o3), (valley, o4)]:
        print(revenue_report(ag, o))
    for ag, g in [(harbor, g1), (ridge, g2), (lakeshore, g3), (valley, g4)]:
        print(traffic_report(ag, g))
    for ag, o in [(harbor, o1), (ridge, o2), (lakeshore, o3), (valley, o4)]:
        print(accounts_report(ag, ga, o))

    led = Ledger(ga)
    order = [(harbor, g1, o1), (ridge, g2, o2), (lakeshore, g3, o3), (valley, g4, o4)]
    if os.environ.get("TOLL_REVERSE") == "1":
        order.reverse()
    for ag, g, o in order:
        print(post_tolls(led, ag, ga, g, o))
    for line in ledger_summary(led, ga):
        print(line)
    write_text("ledger.log", "\n".join(led.log) + "\n")
    back = read_text("ledger.log")
    rows = len(back.split("\n")) - 2
    print(f"ledger.log: {group3(rows)} entries, {group3(len(back))} bytes, fnv {text_sum(back):08x}")

    for p in ["accounts.csv", "hotlist.csv", "harbor_txns.csv", "ridge_txns.csv", "lakeshore_txns.csv",
              "valley_txns.csv", "valley_txns.bad", "ridge_txns.bad", "ledger.log"]:
        os.remove(p)


def main():
    try:
        run()
    except BatchError as e:
        print(f"toll_clearing failed: {e}")


main()
