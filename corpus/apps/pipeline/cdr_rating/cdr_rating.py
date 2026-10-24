"""pipeline/cdr_rating, the Python mirror: a mobile operator's monthly
rating batch over four regions, from subscriber and call detail record
files to deduplication, rating, fraud scoring, billing, reports and one
shared interconnect settlement with the partner network. It wrote
expected.out.

Run from this directory: python3 -I -B cdr_rating.py > expected.out"""
import os

MASK32 = 4294967295
SUBS = 480
CDRS = 5200
SIMBOX_CALLS = 160
RETRIES = 80
DAYS = 30
DAY_SECS = 86400
TYPES = ["voice", "sms", "data", "topup"]
PLANS = ["prepaid", "basic", "plus", "unlimited", "business"]
# Per plan: the monthly fee and credit limit in cents, the bundle of
# domestic minutes, texts and data in KB (-1 is unlimited), the charging
# increments in seconds (the first, then each next), the data rate in
# cents per MB beyond the bundle and the discount on international calls.
FEE = [0, 1500, 3000, 6000, 4500]
CREDIT = [0, 5000, 12000, 20000, 40000]
BUNDLE_MIN = [0, 300, 1000, -1, 2000]
BUNDLE_SMS = [0, 100, 500, -1, -1]
BUNDLE_KB = [0, 2097152, 10485760, 52428800, 20971520]
FIRST_INC = [60, 60, 30, 1, 1]
NEXT_INC = [60, 60, 6, 1, 1]
DATA_RATE = [20, 10, 10, 0, 10]
INTL_OFF = [0, 0, 0, 0, 20]
# The tariff: a destination's class is its longest matching prefix; the
# rates are cents per minute at peak and off peak.
TARIFF = [
    ("7", "onnet", 6, 3),
    ("8", "offnet", 14, 9),
    ("2", "fixed", 8, 5),
    ("3", "fixed", 8, 5),
    ("4", "fixed", 8, 5),
    ("5", "fixed", 8, 5),
    ("800", "tollfree", 0, 0),
    ("900", "premium", 199, 199),
    ("00", "intl-world", 150, 150),
    ("001", "intl-na", 25, 25),
    ("0044", "intl-europe", 45, 40),
    ("0049", "intl-europe", 45, 40),
    ("0033", "intl-europe", 45, 40),
    ("0091", "intl-asia", 60, 50),
    ("0086", "intl-asia", 60, 50),
    ("00881", "satellite", 900, 900),
]
CLASSES = ["onnet", "offnet", "fixed", "tollfree", "premium", "intl-world", "intl-na", "intl-europe", "intl-asia", "satellite", "roaming"]
INTL_CC = ["1", "44", "49", "33", "91", "86", "61", "881"]
ROAM_CC = ["44", "49", "33", "1"]
ROAM_VOICE = 99
ROAM_SMS = 30
ROAM_DATA = 50
SMS_RATE = 10
SMS_INTL = 25
SMS_PREMIUM = 99
TAX_BP = 825
# Fraud points.
SHORT_CALL = 90
SIMBOX_DISTINCT = 100
FLAG_AT = 50
WATCH_AT = 30
# The interconnect agreement with the partner network that terminates
# the off-net calls of all four regions: a rate per minute in tenths of a
# cent that falls at each cumulative volume tier, and a volume rebate,
# once, when the month's minutes pass REBATE_AT.
TIER_AT = [6000, 12000]
TIER_RATE = [12, 9, 6]
REBATE_AT = 15000
REBATE_TENTHS = 2


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


def pct(a, b):
    if b == 0:
        return "0.0%"
    q = a * 1000 // b
    return f"{q // 10}.{q % 10}%"


def gb(kb):
    q = kb * 100 // 1048576
    return f"{q // 100}.{q % 100:02} GB"


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


def stamp(s):
    d = s // DAY_SECS
    r = s % DAY_SECS
    return f"2026-09-{d + 1:02} {r // 3600:02}:{r % 3600 // 60:02}:{r % 60:02}"


def date_of(d):
    return f"2026-09-{d + 1:02}"


# The seconds since 2026-09-01 00:00:00 of a timestamp in September 2026,
# or None.
def secs_of(t):
    if len(t) != 19 or t[4] != "-" or t[7] != "-" or t[10] != " " or t[13] != ":" or t[16] != ":":
        return None
    parts = [t[0:4], t[5:7], t[8:10], t[11:13], t[14:16], t[17:19]]
    if not all(is_digits(p) for p in parts):
        return None
    y, m, d, hh, mm, ss = (int(p) for p in parts)
    if y != 2026 or m != 9 or d < 1 or d > DAYS or hh > 23 or mm > 59 or ss > 59:
        return None
    return (d - 1) * DAY_SECS + hh * 3600 + mm * 60 + ss


def day_of(t):
    if len(t) != 10 or not t.startswith("2026-09-") or not is_digits(t[8:10]):
        return None
    d = int(t[8:10])
    if d < 1 or d > DAYS:
        return None
    return d - 1


def is_peak(s):
    day = s // DAY_SECS
    weekday = (1 + day) % 7
    r = s % DAY_SECS
    return weekday < 5 and 8 * 3600 <= r < 20 * 3600


class Region:
    def __init__(self, name, code, digit, seed, plan_pct):
        self.name = name
        self.code = code
        self.digit = digit
        self.seed = seed
        self.plan_pct = plan_pct


def msisdn(digit, k):
    return f"7{digit}{100000 + k * 13:06}"


def pick_plan(rg, r):
    x = r % 100
    acc = 0
    for p in range(5):
        acc += rg.plan_pct[p]
        if x < acc:
            return p
    return 4


# ------------------------------------------------------------ the books

def build_subs(rg):
    out = []
    for i in range(SUBS):
        plan = pick_plan(rg, mix(rg.seed, i, 1))
        r2 = mix(rg.seed, i, 2)
        act = 0 if r2 % 10 != 0 else 1 + r2 % 28
        bal = 200 + mix(rg.seed, i, 3) % 2000 if plan == 0 else 0
        out.append({"msisdn": msisdn(rg.digit, i), "plan": plan, "act": act, "limit": CREDIT[plan], "bal": bal})
    return out


def simbox_subs(rg):
    return [rg.seed * 7 % SUBS, rg.seed * 11 % SUBS]


def destination(rg, i):
    d = mix(rg.seed, i, 5) % 100
    r = mix(rg.seed, i, 6)
    if d < 38 or d >= 95:
        digit = rg.digit if r % 3 != 0 else 1 + r % 4
        return msisdn(digit, r % SUBS)
    if d < 62:
        return "8" + f"{r % 10000000:07}"
    if d < 78:
        return str(2 + r % 4) + f"{r % 10000000:07}"
    if d < 88:
        return "00" + INTL_CC[r % 8] + f"{r % 100000000:08}"
    if d < 91:
        return "900" + f"{r % 100000:05}"
    return "800" + f"{r % 100000:05}"


def build_cdrs(rg, subs):
    out = []
    for i in range(CDRS):
        r = mix(rg.seed, i, 1)
        s = r % 60 if r % 4 == 0 else r % SUBS
        sub = subs[s]
        t = mix(rg.seed, i, 2) % 100
        typ = 0 if t < 56 else 1 if t < 80 else 2 if t < 97 else 3
        if typ == 3 and sub["plan"] != 0:
            typ = 2
        day = mix(rg.seed, i, 3) % DAYS
        r5 = mix(rg.seed, i, 4)
        sec = 8 * 3600 + r5 % (12 * 3600) if r5 % 10 < 6 else r5 % DAY_SECS
        r7 = mix(rg.seed, i, 7)
        b = "-"
        if typ == 0:
            b = destination(rg, i)
            if r7 % 50 == 0:
                amount = 0
            elif r7 % 10 == 0:
                amount = 600 + r7 % 3000
            else:
                amount = 5 + r7 % 400
        elif typ == 1:
            b = destination(rg, i)
            amount = 1
        elif typ == 2:
            amount = 50000 + r7 % 400000 if r7 % 8 == 0 else 10 + r7 % 30000
        else:
            amount = [500, 1000, 2000, 5000][r7 % 4]
        r8 = mix(rg.seed, i, 8)
        roam = ROAM_CC[r8 % 4] if typ != 3 and r8 % 100 < 5 else "-"
        sw = f"MSC{1 + mix(rg.seed, i, 9) % 3}"
        out.append({"id": "", "type": typ, "a": sub["msisdn"], "b": b, "start": day * DAY_SECS + sec, "amount": amount, "roam": roam, "switch": sw})
    # Two SIM boxes: lines that turn calls from the partner network into
    # cheap on-net minutes make many short calls to distinct numbers.
    for k, s in enumerate(simbox_subs(rg)):
        for j in range(SIMBOX_CALLS):
            m = mix(rg.seed, j, 20 + k)
            out.append({
                "id": "", "type": 0, "a": subs[s]["msisdn"],
                "b": "8" + f"{(rg.seed * 1000 + j * 7919 + k * 31) % 10000000:07}",
                "start": (j % DAYS) * DAY_SECS + 9 * 3600 + (j * 397 + k * 1201) % (10 * 3600),
                "amount": 12 + m % 50, "roam": "-", "switch": "MSC2",
            })
    # Switch retries: the same record delivered again later.
    for j in range(RETRIES):
        c = dict(out[mix(rg.seed, j, 40) % CDRS])
        out.append(c)
    for n, c in enumerate(out):
        c["id"] = f"{rg.code}{n + 1:06}"
    return out


SUB_HEADER = "msisdn,plan,activated,credit_limit,balance"
CDR_HEADER = "id,type,a,b,start,amount,roam,switch"


def encode_subs(ss):
    out = [SUB_HEADER]
    for s in ss:
        out.append(f"{s['msisdn']},{PLANS[s['plan']]},{date_of(s['act'])},{dollars(s['limit'])},{dollars(s['bal'])}")
    return "\n".join(out) + "\n"


def encode_cdrs(cs):
    out = [CDR_HEADER]
    for c in cs:
        out.append(f"{c['id']},{TYPES[c['type']]},{c['a']},{c['b']},{stamp(c['start'])},{c['amount']},{c['roam']},{c['switch']}")
    return "\n".join(out) + "\n"


def read_text(path):
    try:
        with open(path, "r", newline="") as f:
            return f.read()
    except OSError:
        raise BatchError(f"could not read {path}")


def write_text(path, text):
    with open(path, "w", newline="") as f:
        f.write(text)


def damage(text, n, field, value):
    lines = text.split("\n")
    f = lines[n].split(",")
    f[field] = value
    lines[n] = ",".join(f)
    return "\n".join(lines)


def msisdn_ok(t, digit):
    return len(t) == 8 and t.startswith(f"7{digit}") and is_digits(t)


def parse_subs(path, rg, text):
    lines = text.split("\n")
    if lines[0] != SUB_HEADER:
        raise BatchError(f"{path}: bad header")
    out = []
    for k in range(1, len(lines)):
        line = lines[k]
        if line == "":
            continue
        n = k + 1
        f = line.split(",")
        if len(f) != 5:
            raise BatchError(f"{path} line {n}: {len(f)} fields")
        if not msisdn_ok(f[0], rg.digit):
            raise BatchError(f"{path} line {n}: bad number '{f[0]}'")
        if f[1] not in PLANS:
            raise BatchError(f"{path} line {n}: bad plan '{f[1]}'")
        act = day_of(f[2])
        if act is None:
            raise BatchError(f"{path} line {n}: bad activation '{f[2]}'")
        limit = cents_of(f[3])
        if limit is None:
            raise BatchError(f"{path} line {n}: bad credit limit '{f[3]}'")
        bal = cents_of(f[4])
        if bal is None:
            raise BatchError(f"{path} line {n}: bad balance '{f[4]}'")
        out.append({"msisdn": f[0], "plan": PLANS.index(f[1]), "act": act, "limit": limit, "bal": bal})
    return out


def parse_cdrs(path, rg, text):
    lines = text.split("\n")
    if lines[0] != CDR_HEADER:
        raise BatchError(f"{path}: bad header")
    out = []
    for k in range(1, len(lines)):
        line = lines[k]
        if line == "":
            continue
        n = k + 1
        f = line.split(",")
        if len(f) != 8:
            raise BatchError(f"{path} line {n}: {len(f)} fields")
        if len(f[0]) != 7 or f[0][0] != rg.code or not is_digits(f[0][1:]):
            raise BatchError(f"{path} line {n}: bad record id '{f[0]}'")
        if f[1] not in TYPES:
            raise BatchError(f"{path} line {n}: bad type '{f[1]}'")
        typ = TYPES.index(f[1])
        if not msisdn_ok(f[2], rg.digit):
            raise BatchError(f"{path} line {n}: caller '{f[2]}' is not a {rg.name} number")
        if typ < 2:
            if not is_digits(f[3]) or len(f[3]) < 4 or len(f[3]) > 15:
                raise BatchError(f"{path} line {n}: bad destination '{f[3]}'")
        elif f[3] != "-":
            raise BatchError(f"{path} line {n}: a {f[1]} record with a destination")
        start = secs_of(f[4])
        if start is None:
            raise BatchError(f"{path} line {n}: bad start '{f[4]}'")
        if not is_digits(f[5]):
            raise BatchError(f"{path} line {n}: bad amount '{f[5]}'")
        amount = int(f[5])
        if (typ == 0 and amount > DAY_SECS) or (typ == 1 and amount != 1) or (typ >= 2 and amount == 0):
            raise BatchError(f"{path} line {n}: bad amount '{f[5]}' for {f[1]}")
        if f[6] != "-" and (not is_digits(f[6]) or len(f[6]) > 3):
            raise BatchError(f"{path} line {n}: bad roaming country '{f[6]}'")
        if len(f[7]) != 4 or not f[7].startswith("MSC") or f[7][3] not in "123":
            raise BatchError(f"{path} line {n}: bad switch '{f[7]}'")
        out.append({"id": f[0], "type": typ, "a": f[2], "b": f[3], "start": start, "amount": amount, "roam": f[6], "switch": f[7]})
    return out


SUB_KEYS = ["msisdn", "plan", "act", "limit", "bal"]
CDR_KEYS = ["id", "type", "a", "b", "start", "amount", "roam", "switch"]


def same(a, b, keys):
    return len(a) == len(b) and all(all(x[k] == y[k] for k in keys) for x, y in zip(a, b))


# ------------------------------------------------------------ dedupe

def dedupe(cs):
    seen = {}
    out = []
    for c in cs:
        key = f"{c['type']}|{c['a']}|{c['b']}|{c['start']}|{c['amount']}"
        if key in seen:
            continue
        seen[key] = len(out)
        out.append(c)
    return out


# ------------------------------------------------------------ rating

def classify(b):
    best = -1
    best_len = 0
    for k, (prefix, _, _, _) in enumerate(TARIFF):
        if len(prefix) > best_len and b.startswith(prefix):
            best = k
            best_len = len(prefix)
    return best


def class_index(name):
    return CLASSES.index(name)


def billed_secs(d, first, nxt):
    if d == 0:
        return 0
    if d <= first:
        return first
    return first + ceil_div(d - first, nxt) * nxt


def order_by(keys):
    return sorted(range(len(keys)), key=lambda i: (keys[i], i))


def sub_index(subs):
    return {s["msisdn"]: i for i, s in enumerate(subs)}


CATS = ["voice", "sms", "data", "intl", "premium", "roaming"]


def rate(subs, cs):
    idx = sub_index(subs)
    n = len(subs)
    mins_left = [BUNDLE_MIN[s["plan"]] * 60 for s in subs]
    sms_left = [BUNDLE_SMS[s["plan"]] for s in subs]
    kb_left = [BUNDLE_KB[s["plan"]] for s in subs]
    bal = [s["bal"] for s in subs]
    usage = [[0] * 6 for _ in range(n)]
    topped = [0] * n
    blocked = [0] * n
    charge = [0] * len(cs)
    cls = [-1] * len(cs)
    status = [0] * len(cs)
    covered_secs = 0
    for i in order_by([c["start"] for c in cs]):
        c = cs[i]
        si = idx[c["a"]]
        sub = subs[si]
        plan = sub["plan"]
        if c["start"] // DAY_SECS < sub["act"]:
            status[i] = 2
            continue
        if c["type"] == 3:
            bal[si] += c["amount"]
            topped[si] += c["amount"]
            continue
        cat = 0
        ch = 0
        roaming = c["roam"] != "-"
        if c["type"] == 0:
            billed = billed_secs(c["amount"], FIRST_INC[plan], NEXT_INC[plan])
            if roaming:
                cls[i] = class_index("roaming")
                cat = 5
                ch = ceil_div(billed * ROAM_VOICE, 60)
            else:
                k = classify(c["b"])
                prefix, name, peak, off = TARIFF[k]
                cls[i] = class_index(name)
                r = peak if is_peak(c["start"]) else off
                if name in ("onnet", "offnet", "fixed"):
                    rest = billed
                    if BUNDLE_MIN[plan] < 0:
                        rest = 0
                    elif mins_left[si] > 0:
                        cover = min(billed, mins_left[si])
                        mins_left[si] -= cover
                        rest = billed - cover
                    covered_secs += billed - rest
                    ch = ceil_div(rest * r, 60)
                elif name == "premium":
                    cat = 4
                    ch = ceil_div(billed * r, 60)
                elif name == "tollfree":
                    ch = 0
                else:
                    cat = 3
                    ch = ceil_div(billed * r, 60)
                    ch -= ch * INTL_OFF[plan] // 100
        elif c["type"] == 1:
            cat = 1
            if roaming:
                cat = 5
                ch = ROAM_SMS
            elif c["b"].startswith("00"):
                cat = 3
                ch = SMS_INTL
            elif c["b"].startswith("900"):
                cat = 4
                ch = SMS_PREMIUM
            elif BUNDLE_SMS[plan] < 0:
                ch = 0
            elif sms_left[si] > 0:
                sms_left[si] -= 1
                ch = 0
            else:
                ch = SMS_RATE
        else:
            cat = 2
            units = ceil_div(c["amount"], 10) * 10
            if roaming:
                cat = 5
                ch = ceil_div(units * ROAM_DATA, 1024)
            else:
                cover = min(units, kb_left[si])
                kb_left[si] -= cover
                ch = ceil_div((units - cover) * DATA_RATE[plan], 1024)
        if plan == 0:
            if ch > bal[si]:
                status[i] = 1
                blocked[si] += 1
                continue
            bal[si] -= ch
        charge[i] = ch
        usage[si][cat] += ch
    return {"usage": usage, "bal": bal, "topped": topped, "blocked": blocked, "charge": charge, "cls": cls, "status": status, "covered": covered_secs}


# ------------------------------------------------------------ fraud

def score(subs, cs, rt):
    idx = sub_index(subs)
    n = len(subs)
    pairs = {}
    distinct = [0] * n
    short = [0] * n
    calls = [0] * n
    for i, c in enumerate(cs):
        if c["type"] != 0 or rt["status"][i] != 0 or c["roam"] != "-":
            continue
        si = idx[c["a"]]
        calls[si] += 1
        if c["amount"] < SHORT_CALL and c["b"][0] in "82345":
            short[si] += 1
            key = f"{c['a']}|{c['b']}"
            if key not in pairs:
                pairs[key] = 1
                distinct[si] += 1
    pts = [0] * n
    reasons = [[] for _ in range(n)]
    for si in range(n):
        u = rt["usage"][si]
        if distinct[si] >= SIMBOX_DISTINCT:
            pts[si] += 60
            reasons[si].append(f"simbox x{distinct[si]}")
        if calls[si] >= 20 and short[si] * 100 >= calls[si] * 70:
            pts[si] += 20
            reasons[si].append("short")
        if u[4] > 3000:
            pts[si] += 30
            reasons[si].append("premium")
        if u[5] > 5000:
            pts[si] += 15
            reasons[si].append("roaming")
        if subs[si]["plan"] == 0 and rt["blocked"][si] >= 5:
            pts[si] += 10
            reasons[si].append("refused")
    return {"pts": pts, "reasons": reasons}


# ------------------------------------------------------------ billing

def bill(subs, rt):
    out = []
    for si, s in enumerate(subs):
        plan = s["plan"]
        if plan == 0:
            out.append(None)
            continue
        fee = FEE[plan] * (DAYS - s["act"]) // DAYS
        usage = sum(rt["usage"][si])
        sub = fee + usage
        tax = (sub * TAX_BP + 5000) // 10000
        total = sub + tax
        out.append({"fee": fee, "usage": usage, "tax": tax, "total": total, "barred": total > s["limit"]})
    return out


# ------------------------------------------------------------ reports

def top3(counts, names):
    order = sorted(range(len(counts)), key=lambda k: (-counts[k], k))[:3]
    return ", ".join(f"{names[k]} {counts[k]}" for k in order if counts[k] > 0)


def traffic_report(rg, raw, cs, rt):
    calls = mins = peak = texts = kb = tops = topped = 0
    for i, c in enumerate(cs):
        if c["type"] == 0:
            calls += 1
            mins += ceil_div(c["amount"], 60)
            if is_peak(c["start"]):
                peak += 1
        elif c["type"] == 1:
            texts += 1
        elif c["type"] == 2:
            kb += c["amount"]
        else:
            tops += 1
            topped += c["amount"]
    before = sum(1 for s in rt["status"] if s == 2)
    return (f"{rg.name} traffic: {group3(len(raw))} records, {len(raw) - len(cs)} switch retries dropped, {before} before activation; "
            f"{group3(calls)} calls for {group3(mins)} minutes ({pct(peak, calls)} at peak), {group3(texts)} texts, {gb(kb)}, {tops} top-ups for {money(topped)}")


def class_report(rg, cs, rt):
    mins = [0] * len(CLASSES)
    rev = [0] * len(CLASSES)
    for i, c in enumerate(cs):
        k = rt["cls"][i]
        if k < 0 or rt["status"][i] != 0:
            continue
        mins[k] += ceil_div(c["amount"], 60)
        rev[k] += rt["charge"][i]
    order = sorted(range(len(CLASSES)), key=lambda k: (-rev[k], k))[:3]
    revs = ", ".join(f"{CLASSES[k]} {money(rev[k])}" for k in order)
    return f"{rg.name} classes: most minutes {top3(mins, CLASSES)}; most revenue {revs}; {group3(rt['covered'] // 60)} bundle minutes used"


def billing_report(rg, subs, bills):
    n = fee = usage = tax = total = barred = 0
    top = -1
    for si, b in enumerate(bills):
        if b is None:
            continue
        n += 1
        fee += b["fee"]
        usage += b["usage"]
        tax += b["tax"]
        total += b["total"]
        if b["barred"]:
            barred += 1
        if top < 0 or b["total"] > bills[top]["total"]:
            top = si
    arpu = total // n if n else 0
    return (f"{rg.name} postpaid: {n} bills for {money(total)} (fees {money(fee)}, usage {money(usage)}, tax {money(tax)}), ARPU {money(arpu)}; "
            f"{barred} over their credit limit; largest {subs[top]['msisdn']} {money(bills[top]['total'])}")


def prepaid_report(rg, subs, rt):
    n = topped = spent = refused = empty = 0
    for si, s in enumerate(subs):
        if s["plan"] != 0:
            continue
        n += 1
        topped += rt["topped"][si]
        spent += sum(rt["usage"][si])
        refused += rt["blocked"][si]
        if rt["bal"][si] < 10:
            empty += 1
    held = sum(rt["bal"][si] for si, s in enumerate(subs) if s["plan"] == 0)
    return f"{rg.name} prepaid: {n} lines, {money(topped)} topped up, {money(spent)} spent, {refused} events refused for credit, {empty} lines under 10 cents; {money(held)} held"


def fraud_report(rg, subs, fr):
    pts = fr["pts"]
    flagged = sum(1 for p in pts if p >= FLAG_AT)
    watch = sum(1 for p in pts if WATCH_AT <= p < FLAG_AT)
    order = sorted(range(len(subs)), key=lambda k: (-pts[k], k))[:3]
    tops = "; ".join(f"{subs[k]['msisdn']} {pts[k]} ({' '.join(fr['reasons'][k])})" for k in order)
    return f"{rg.name} fraud: {flagged} flagged, {watch} to watch; highest {tops}"


# ------------------------------------------------------------ interconnect

class Interconnect:
    def __init__(self):
        self.minutes = 0
        self.tenths = 0
        self.invoices = 0
        self.credits = 0
        self.rebated = False
        self.log = []


def tier_of(m):
    t = 0
    while t < len(TIER_AT) and m >= TIER_AT[t]:
        t += 1
    return t


def settle(ic, rg, cs, rt):
    keys = []
    ix = []
    for i, c in enumerate(cs):
        if rt["cls"][i] == class_index("offnet") and rt["status"][i] == 0 and c["amount"] > 0:
            keys.append(c["start"])
            ix.append(i)
    calls = 0
    mins = 0
    tenths = 0
    first = -1
    week = -1
    w_calls = w_mins = w_tenths = 0
    notes = []

    def close():
        if w_calls > 0:
            ic.invoices += 1
            iid = f"IC-{ic.invoices:04}"
            ic.log.append(f"{iid},{rg.name},week {week + 1},{w_calls},{w_mins},{dollars((w_tenths + 5) // 10)}")
            return iid
        return None

    for k in order_by(keys):
        i = ix[k]
        c = cs[i]
        wk = min(c["start"] // DAY_SECS // 7, 3)
        if wk != week:
            iid = close()
            if iid is not None and first < 0:
                first = ic.invoices
            week = wk
            w_calls = w_mins = w_tenths = 0
        m = ceil_div(c["amount"], 60)
        fee = 0
        for _ in range(m):
            fee += TIER_RATE[tier_of(ic.minutes)]
            ic.minutes += 1
        calls += 1
        mins += m
        tenths += fee
        w_calls += 1
        w_mins += m
        w_tenths += fee
        ic.tenths += fee
        if not ic.rebated and ic.minutes >= REBATE_AT:
            ic.rebated = True
            ic.credits += 1
            back = ic.minutes * REBATE_TENTHS
            ic.tenths -= back
            ic.log.append(f"CN-{ic.credits:04},{rg.name},{stamp(c['start'])},{ic.minutes},-{dollars((back + 5) // 10)}")
            notes.append(f"rebate CN-{ic.credits:04} of {money((back + 5) // 10)} at {stamp(c['start'])}")
    iid = close()
    if iid is not None and first < 0:
        first = ic.invoices
    n_inv = ic.invoices - first + 1 if first > 0 else 0
    inv = f"invoices IC-{first:04} to IC-{ic.invoices:04}" if n_inv > 1 else f"invoice IC-{first:04}" if n_inv == 1 else "no invoices"
    extra = "; " + "; ".join(notes) if notes else ""
    return (f"{rg.name} interconnect: {calls} calls, {group3(mins)} minutes, {money((tenths + 5) // 10)} in {inv}{extra}; "
            f"month to date {group3(ic.minutes)} minutes in tier {tier_of(ic.minutes) + 1}, owed {money((ic.tenths + 5) // 10)}")


# ------------------------------------------------------------ the job

def run():
    regions = [
        Region("north", "N", 1, 13, [30, 25, 20, 10, 15]),
        Region("coast", "C", 2, 29, [40, 20, 20, 10, 10]),
        Region("valley", "V", 3, 41, [45, 30, 15, 5, 5]),
        Region("metro", "M", 4, 59, [20, 15, 25, 20, 20]),
    ]
    subs = [build_subs(r) for r in regions]
    cdrs = [build_cdrs(r, s) for r, s in zip(regions, subs)]
    for r, s in zip(regions, subs):
        write_text(f"{r.name}_subs.csv", encode_subs(s))
    for r, c in zip(regions, cdrs):
        write_text(f"{r.name}_cdrs.csv", encode_cdrs(c))

    whole = read_text("coast_cdrs.csv")
    write_text("coast_cdrs.bad", damage(whole, 2077, 4, "2026-09-31 10:00:00"))

    ts = [read_text(f"{r.name}_subs.csv") for r in regions]
    tc = [read_text(f"{r.name}_cdrs.csv") for r in regions]
    for r, a, b in zip(regions, ts, tc):
        print(f"{r.name}: subscribers {group3(len(a))} bytes, fnv {text_sum(a):08x}; records {group3(len(b))} bytes, fnv {text_sum(b):08x}")

    gs = [parse_subs(f"{r.name}_subs.csv", r, t) for r, t in zip(regions, ts)]
    gc = [parse_cdrs(f"{r.name}_cdrs.csv", r, t) for r, t in zip(regions, tc)]
    for path, rg, text in [("coast_cdrs.bad", regions[1], read_text("coast_cdrs.bad")), ("metro_cdrs.csv", regions[0], tc[3])]:
        try:
            parse_cdrs(path, rg, text)
            print(f"{path}: parsed")
        except BatchError as e:
            print(e)
    ok = all(same(a, b, SUB_KEYS) for a, b in zip(gs, subs)) and all(same(a, b, CDR_KEYS) for a, b in zip(gc, cdrs))
    print(f"parsed files match the built ones: {'true' if ok else 'false'}")

    dd = [dedupe(c) for c in gc]
    rt = [rate(s, c) for s, c in zip(gs, dd)]
    fr = [score(s, c, t) for s, c, t in zip(gs, dd, rt)]
    bl = [bill(s, t) for s, t in zip(gs, rt)]

    lines = []
    for r, raw, c, t in zip(regions, gc, dd, rt):
        lines.append(traffic_report(r, raw, c, t))
    for r, c, t in zip(regions, dd, rt):
        lines.append(class_report(r, c, t))
    for r, s, b in zip(regions, gs, bl):
        lines.append(billing_report(r, s, b))
    for r, s, t in zip(regions, gs, rt):
        lines.append(prepaid_report(r, s, t))
    for r, s, f in zip(regions, gs, fr):
        lines.append(fraud_report(r, s, f))
    for line in lines:
        print(line)

    ic = Interconnect()
    for r, c, t in zip(regions, dd, rt):
        print(settle(ic, r, c, t))
    write_text("settle.log", "\n".join(ic.log) + "\n")
    back = read_text("settle.log")
    rows = len(back.split("\n")) - 1
    print(f"settle.log: {rows} entries, {group3(len(back))} bytes, fnv {text_sum(back):08x}")

    for r in regions:
        os.remove(f"{r.name}_subs.csv")
        os.remove(f"{r.name}_cdrs.csv")
    os.remove("coast_cdrs.bad")
    os.remove("settle.log")


if __name__ == "__main__":
    try:
        run()
    except BatchError as e:
        print(f"cdr_rating failed: {e}")
