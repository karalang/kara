"""pipeline/rx_adjudicate, the Python mirror: a pharmacy benefit manager's
month-end batch over four pharmacies, from the formulary, member roster
and claim files to the edits that reject a claim, the pricing that picks
the lesser of the discounted AWP, the MAC and the pharmacy's usual and
customary price, the per-pharmacy reports, and one shared set of member
accumulators that splits each paid claim between member and plan. It
wrote expected.out.

Run from this directory: python3 -I -B rx_adjudicate.py > expected.out"""
import os

MASK32 = 4294967295
# Day 0 is Thursday 1 October 2026.
FIRST_WEEKDAY = 3
WEEKDAYS = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"]
MONTH_START = [0, 31, 59, 90, 120, 151, 181, 212, 243, 273, 304, 334]
MONTH_DAYS = [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
DAY0 = 273
MONTH_LEN = 31
SURNAMES = ["Abara", "Brandt", "Castillo", "Dubois", "Eriksen", "Fontaine", "Garcia", "Haddad", "Ivanova", "Jensen",
            "Kowalski", "Laurent", "Moreau", "Nakamura", "Okafor", "Petrov", "Quinn", "Rossi", "Sato", "Tanaka",
            "Ueda", "Varga", "Weber", "Xu", "Yilmaz", "Zhang"]
PREFIXES = ["ator", "lisin", "metfor", "amlo", "sertra", "omepra", "levo", "gaba", "losar", "rosuva", "escita", "monte"]
SUFFIXES = ["statin", "pril", "mide", "zole"]
LABELERS = [93, 378, 591, 781, 50090, 68180]
DRUGS = 48
MEMBERS = 240
PRESCRIBERS = 40
DAYS_CHOICES = [30, 30, 30, 30, 90, 90, 7, 14]
# Classes that interact: a fill of one while a fill of the other is active.
INTERACTS = [3, -1, 5, 0, 7, 2, -1, 4]
REJECTS = ["65", "70", "75", "76", "79", "88"]
REJECT_NAMES = ["patient not covered", "product not covered", "prior authorization required",
                "plan limits exceeded", "refill too soon", "drug utilization review"]
UNITS_A_DAY = 4
SCHEDULES = [0, 2, 3, 4, 5]
DUR_CODES = ["", "M0", "R0"]


class Plan:
    def __init__(self, name, ded, copay, coins, oop):
        self.name = name
        self.ded = ded
        self.copay = copay
        self.coins = coins
        self.oop = oop


PLANS = [Plan("bronze", 150000, [1000, 4000, 8000], 30, 600000),
         Plan("silver", 50000, [500, 3000, 6000], 25, 400000),
         Plan("gold", 0, [300, 2000, 4500], 20, 250000)]


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


def plural(n, word):
    return f"{n} {word}" if n == 1 else f"{n} {word}s"


def is_digits(t):
    return len(t) > 0 and all("0" <= ch <= "9" for ch in t)


def is_upper(ch):
    return "A" <= ch <= "Z"


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


def dea_check(six):
    d = [int(ch) for ch in six]
    return (d[0] + d[2] + d[4] + 2 * (d[1] + d[3] + d[5])) % 10


def dea_ok(t):
    if len(t) != 9 or not is_upper(t[0]) or not is_upper(t[1]) or not is_digits(t[2:]):
        return False
    return dea_check(t[2:8]) == int(t[8])


# ------------------------------------------------------------ the data

class Drug:
    def __init__(self, ndc, name, tier, awp, mac, gen_awp, gen_mac, schedule, cls, covered):
        self.ndc = ndc
        self.name = name
        self.tier = tier
        self.awp = awp
        self.mac = mac
        self.gen_awp = gen_awp
        self.gen_mac = gen_mac
        self.schedule = schedule
        self.cls = cls
        self.covered = covered

    def key(self):
        return (self.ndc, self.name, self.tier, self.awp, self.mac, self.gen_awp, self.gen_mac,
                self.schedule, self.cls, self.covered)


class Member:
    def __init__(self, mid, name, plan, ytd_ded, ytd_oop, term, pa):
        self.mid = mid
        self.name = name
        self.plan = plan
        self.ytd_ded = ytd_ded
        self.ytd_oop = ytd_oop
        self.term = term
        self.pa = pa

    def key(self):
        return (self.mid, self.name, self.plan, self.ytd_ded, self.ytd_oop, self.term, self.pa)


class Claim:
    def __init__(self, rx, member, ndc, day, qty, days, refill, dea, daw, dur):
        self.rx = rx
        self.member = member
        self.ndc = ndc
        self.day = day
        self.qty = qty
        self.days = days
        self.refill = refill
        self.dea = dea
        self.daw = daw
        self.dur = dur

    def key(self):
        return (self.rx, self.member, self.ndc, self.day, self.qty, self.days, self.refill, self.dea,
                self.daw, self.dur)


class Pharmacy:
    def __init__(self, name, code, seed, fee, disc_bp, uc_pct, scripts):
        self.name = name
        self.code = code
        self.seed = seed
        self.fee = fee
        self.disc_bp = disc_bp
        self.uc_pct = uc_pct
        self.scripts = scripts


def build_formulary():
    out = []
    for i in range(DRUGS):
        h = mix(7, i, 1) % 100
        tier = 1 if h < 45 else 2 if h < 75 else 3 if h < 92 else 4
        h2 = mix(7, i, 2)
        if tier == 1:
            awp = 8 + h2 % 60
        elif tier == 2:
            awp = 150 + h2 % 600
        elif tier == 3:
            awp = 300 + h2 % 1200
        else:
            awp = 20000 + h2 % 60000
        h3 = mix(7, i, 3)
        mac = awp * (30 + h3 % 30) // 100 if tier == 1 else 0
        gen_awp = 0
        gen_mac = 0
        if (tier == 2 or tier == 3) and h3 % 3 == 0:
            gen_awp = awp * 30 // 100
            gen_mac = gen_awp * 60 // 100
        h4 = mix(7, i, 4)
        schedule = 0
        if tier < 4 and h4 % 10 == 0:
            schedule = 2
        elif tier < 4 and h4 % 10 == 1:
            schedule = 4
        cls = (h4 // 10) % 8
        covered = mix(7, i, 5) % 17 != 0
        ndc = f"{LABELERS[i % 6]:05}{1000 + i * 7:04}{1 + i % 3:02}"
        out.append(Drug(ndc, PREFIXES[i % 12] + SUFFIXES[i // 12], tier, awp, mac, gen_awp, gen_mac,
                        schedule, cls, covered))
    return out


def build_members(drugs):
    specialty = [d.ndc for d in drugs if d.tier == 4]
    out = []
    for j in range(MEMBERS):
        name = f"{SURNAMES[mix(11, j, 1) % 26]} {chr(65 + mix(11, j, 2) % 26)}."
        plan = mix(11, j, 3) % 3
        p = PLANS[plan]
        ytd_ded = 0
        if p.ded > 0:
            ytd_ded = min(p.ded, mix(11, j, 4) % (p.ded * 3 // 2 + 1))
        ytd_oop = min(p.oop, ytd_ded + mix(11, j, 5) % (p.oop // 2))
        term = None
        if mix(11, j, 6) % 25 == 0:
            term = mix(11, j, 7) % 40 - 9
        pa = " ".join(n for k, n in enumerate(specialty) if mix(13, j * DRUGS + k, 8) % 2 == 0)
        out.append(Member(f"M{j + 1:05}", name, plan, ytd_ded, ytd_oop, term, pa))
    return out


def build_prescribers():
    out = []
    for k in range(PRESCRIBERS):
        surname = SURNAMES[mix(17, k, 1) % 26]
        six = f"{mix(17, k, 2) % 1000000:06}"
        out.append("ABFM"[mix(17, k, 3) % 4] + surname[0] + six + str(dea_check(six)))
    return out


def build_claims(ph, drugs, members, prescribers):
    raw = []
    for s in range(ph.scripts):
        def hs(salt):
            return mix(ph.seed, s, salt)
        mj = (ph.seed * 7 + (hs(1) % 80) * 3) % MEMBERS
        d = drugs[hs(2) % DRUGS]
        days = DAYS_CHOICES[hs(3) % 8]
        if d.schedule == 2:
            days = 60 if hs(9) % 15 == 0 else 30
        if hs(9) % 40 == 1:
            days = 120
        per_day = 1 + hs(4) % 3
        qty = days * per_day
        if hs(4) % 50 == 7:
            qty = days * 5
        day = hs(5) % MONTH_LEN
        refill = hs(6) % 4
        if d.schedule == 2:
            refill = 1 if hs(6) % 10 == 0 else 0
        dea = prescribers[hs(7) % PRESCRIBERS]
        daw = 0
        if d.gen_awp > 0:
            k = hs(8) % 10
            daw = 1 if k == 7 or k == 8 else 2 if k == 9 else 0
        elif hs(8) % 10 == 0:
            daw = 1
        dur = DUR_CODES[[1, 2][hs(20) % 2]] if hs(20) % 12 < 2 else ""
        while day < MONTH_LEN and refill <= 11:
            raw.append((day, members[mj].mid, d.ndc, refill, qty, days, dea, daw, dur))
            if d.schedule == 2:
                break
            h = hs(10 + refill)
            if h % 12 == 0:
                step = days * 55 // 100
            else:
                step = days * (85 + h % 20) // 100
            day += max(1, step)
            refill += 1
    raw.sort()
    out = []
    for n, r in enumerate(raw):
        day, mid, ndc, refill, qty, days, dea, daw, dur = r
        out.append(Claim(f"{ph.code}{ph.seed * 10000 + n + 1:07}", mid, ndc, day, qty, days, refill, dea, daw, dur))
    return out


FORMULARY_HEADER = "ndc,name,tier,awp,mac,generic_awp,generic_mac,schedule,class,covered"
MEMBERS_HEADER = "member,name,plan,ytd_deductible,ytd_oop,terminated,prior_auth"
CLAIMS_HEADER = "rx,member,ndc,fill_date,qty,days,refill,dea,daw,dur"


def opt_dollars(c):
    return dollars(c) if c > 0 else ""


def encode_formulary(drugs):
    lines = [FORMULARY_HEADER]
    for d in drugs:
        lines.append(",".join([d.ndc, d.name, str(d.tier), dollars(d.awp), opt_dollars(d.mac), opt_dollars(d.gen_awp),
                               opt_dollars(d.gen_mac), str(d.schedule), str(d.cls), "Y" if d.covered else "N"]))
    return "\n".join(lines) + "\n"


def encode_members(members):
    lines = [MEMBERS_HEADER]
    for m in members:
        term = "" if m.term is None else date_of(m.term)
        lines.append(",".join([m.mid, m.name, PLANS[m.plan].name, dollars(m.ytd_ded), dollars(m.ytd_oop), term, m.pa]))
    return "\n".join(lines) + "\n"


def encode_claims(claims):
    lines = [CLAIMS_HEADER]
    for c in claims:
        lines.append(",".join([c.rx, c.member, c.ndc, date_of(c.day), str(c.qty), str(c.days), str(c.refill), c.dea,
                               str(c.daw), c.dur]))
    return "\n".join(lines) + "\n"


def read_text(path):
    try:
        with open(path, encoding="utf-8") as f:
            return f.read()
    except OSError:
        raise BatchError(f"cannot read {path}")


def write_text(path, text):
    with open(path, "w", encoding="utf-8") as f:
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


def opt_money_field(path, n, what, t):
    if t == "":
        return 0
    return money_field(path, n, what, t)


def parse_formulary(path, text):
    out = []
    seen = set()
    for k, line in enumerate(body_lines(path, text, FORMULARY_HEADER)):
        n = k + 2
        f = line.split(",")
        if len(f) != 10:
            raise BatchError(f"{path} line {n}: {len(f)} fields, not 10")
        if len(f[0]) != 11 or not is_digits(f[0]):
            raise BatchError(f"{path} line {n}: bad ndc '{f[0]}'")
        if f[0] in seen:
            raise BatchError(f"{path} line {n}: ndc {f[0]} listed twice")
        seen.add(f[0])
        if f[1] == "":
            raise BatchError(f"{path} line {n}: no name")
        tier = small_int(f[2], 1, 4)
        if tier is None:
            raise BatchError(f"{path} line {n}: bad tier '{f[2]}'")
        awp = money_field(path, n, "awp", f[3])
        mac = opt_money_field(path, n, "mac", f[4])
        gen_awp = opt_money_field(path, n, "generic awp", f[5])
        gen_mac = opt_money_field(path, n, "generic mac", f[6])
        if (f[5] == "") != (f[6] == ""):
            raise BatchError(f"{path} line {n}: a generic needs both prices")
        schedule = small_int(f[7], 0, 5)
        if schedule is None or schedule not in SCHEDULES:
            raise BatchError(f"{path} line {n}: bad schedule '{f[7]}'")
        cls = small_int(f[8], 0, 7)
        if cls is None:
            raise BatchError(f"{path} line {n}: bad class '{f[8]}'")
        if f[9] != "Y" and f[9] != "N":
            raise BatchError(f"{path} line {n}: bad covered flag '{f[9]}'")
        out.append(Drug(f[0], f[1], tier, awp, mac, gen_awp, gen_mac, schedule, cls, f[9] == "Y"))
    return out


def plan_index(t):
    for i, p in enumerate(PLANS):
        if p.name == t:
            return i
    return -1


def parse_members(path, text):
    out = []
    seen = set()
    for k, line in enumerate(body_lines(path, text, MEMBERS_HEADER)):
        n = k + 2
        f = line.split(",")
        if len(f) != 7:
            raise BatchError(f"{path} line {n}: {len(f)} fields, not 7")
        if len(f[0]) != 6 or f[0][0] != "M" or not is_digits(f[0][1:]):
            raise BatchError(f"{path} line {n}: bad member '{f[0]}'")
        if f[0] in seen:
            raise BatchError(f"{path} line {n}: member {f[0]} listed twice")
        seen.add(f[0])
        if f[1] == "":
            raise BatchError(f"{path} line {n}: no name")
        plan = plan_index(f[2])
        if plan < 0:
            raise BatchError(f"{path} line {n}: bad plan '{f[2]}'")
        ded = money_field(path, n, "deductible", f[3])
        oop = money_field(path, n, "out-of-pocket", f[4])
        if ded > PLANS[plan].ded or oop > PLANS[plan].oop or oop < ded:
            raise BatchError(f"{path} line {n}: accumulators out of range")
        term = None
        if f[5] != "":
            term = day_of(f[5])
            if term is None:
                raise BatchError(f"{path} line {n}: bad termination date '{f[5]}'")
        for t in f[6].split(" "):
            if f[6] != "" and (len(t) != 11 or not is_digits(t)):
                raise BatchError(f"{path} line {n}: bad prior authorization '{t}'")
        out.append(Member(f[0], f[1], plan, ded, oop, term, f[6]))
    return out


def parse_claims(path, ph, drugs, members, text):
    known_drugs = {d.ndc for d in drugs}
    known_members = {m.mid for m in members}
    out = []
    for k, line in enumerate(body_lines(path, text, CLAIMS_HEADER)):
        n = k + 2
        f = line.split(",")
        if len(f) != 10:
            raise BatchError(f"{path} line {n}: {len(f)} fields, not 10")
        if len(f[0]) != 8 or f[0][0] != ph.code or not is_digits(f[0][1:]):
            raise BatchError(f"{path} line {n}: bad rx '{f[0]}' for {ph.name}")
        if f[1] not in known_members:
            raise BatchError(f"{path} line {n}: unknown member '{f[1]}'")
        if f[2] not in known_drugs:
            raise BatchError(f"{path} line {n}: unknown ndc '{f[2]}'")
        day = day_of(f[3])
        if day is None or day < 0 or day >= MONTH_LEN:
            raise BatchError(f"{path} line {n}: fill date '{f[3]}' is not in October 2026")
        qty = small_int(f[4], 1, 9999)
        if qty is None:
            raise BatchError(f"{path} line {n}: bad quantity '{f[4]}'")
        days = small_int(f[5], 1, 365)
        if days is None:
            raise BatchError(f"{path} line {n}: bad days supply '{f[5]}'")
        refill = small_int(f[6], 0, 11)
        if refill is None:
            raise BatchError(f"{path} line {n}: bad refill number '{f[6]}'")
        if not dea_ok(f[7]):
            raise BatchError(f"{path} line {n}: bad DEA number '{f[7]}'")
        daw = small_int(f[8], 0, 2)
        if daw is None:
            raise BatchError(f"{path} line {n}: bad DAW code '{f[8]}'")
        if f[9] not in DUR_CODES:
            raise BatchError(f"{path} line {n}: bad DUR code '{f[9]}'")
        out.append(Claim(f[0], f[1], f[2], day, qty, days, refill, f[7], daw, f[9]))
    return out


# ------------------------------------------------------------ adjudication

class Result:
    def __init__(self):
        # the index into REJECTS, or -1 for a paid claim
        self.reject = -1
        self.ingredient = 0
        self.fee = 0
        self.allowed = 0
        self.penalty = 0
        self.basis = ""
        self.generic = False
        self.override = False


def price(ph, awp, mac, qty):
    ing = awp * qty * (10000 - ph.disc_bp) // 10000
    basis = "awp"
    if mac > 0 and mac * qty < ing:
        ing = mac * qty
        basis = "mac"
    gross = ing + ph.fee
    uc = awp * qty * ph.uc_pct // 100 + 500
    if uc < gross:
        return ing, uc, "uc"
    return ing, gross, basis


def adjudicate(ph, drugs, members, claims):
    drug = {d.ndc: d for d in drugs}
    member = {m.mid: m for m in members}
    last = {}
    active = {}
    out = []
    for c in claims:
        d = drug[c.ndc]
        m = member[c.member]
        r = Result()
        inter = False
        for f in active.get(c.member, []):
            if f[0] + f[1] > c.day and INTERACTS[d.cls] == f[2]:
                inter = True
        prev = last.get((c.member, c.ndc))
        if m.term is not None and c.day > m.term:
            r.reject = 0
        elif not d.covered:
            r.reject = 1
        elif c.days > (30 if d.schedule == 2 else 90) or c.qty > c.days * UNITS_A_DAY:
            r.reject = 3
        elif d.schedule == 2 and c.refill > 0:
            r.reject = 5
        elif d.tier == 4 and c.ndc not in m.pa.split(" "):
            r.reject = 2
        elif prev is not None and c.day - prev[0] < prev[1] * 75 // 100:
            r.reject = 4
        elif inter and c.dur == "":
            r.reject = 5
        if r.reject < 0:
            r.override = inter
            if d.tier == 1:
                r.generic = True
                r.ingredient, r.allowed, r.basis = price(ph, d.awp, d.mac, c.qty)
            elif d.gen_awp > 0 and c.daw == 0:
                r.generic = True
                r.ingredient, r.allowed, r.basis = price(ph, d.gen_awp, d.gen_mac, c.qty)
            elif d.gen_awp > 0 and c.daw == 2:
                r.ingredient, r.allowed, r.basis = price(ph, d.gen_awp, d.gen_mac, c.qty)
                brand = price(ph, d.awp, 0, c.qty)[1]
                r.penalty = max(0, brand - r.allowed)
            else:
                r.ingredient, r.allowed, r.basis = price(ph, d.awp, 0, c.qty)
            if r.basis != "uc":
                r.fee = ph.fee
            last[(c.member, c.ndc)] = (c.day, c.days)
            active.setdefault(c.member, []).append((c.day, c.days, d.cls))
        out.append(r)
    return out


# ------------------------------------------------------------ reports

def claims_report(ph, adj):
    counts = [0] * len(REJECTS)
    for r in adj:
        if r.reject >= 0:
            counts[r.reject] += 1
    rej = sum(counts)
    parts = ", ".join(f"{REJECTS[k]} {counts[k]}" for k in range(len(REJECTS)))
    return f"{ph.name} claims: {len(adj)} received, {len(adj) - rej} paid, {rej} rejected ({pct1(rej, len(adj))}); {parts}"


def pricing_report(ph, adj):
    allowed = ing = fees = pen = paid = generic = daw2 = 0
    basis = {"awp": 0, "mac": 0, "uc": 0}
    for r in adj:
        if r.reject >= 0:
            continue
        paid += 1
        allowed += r.allowed
        ing += r.ingredient
        fees += r.fee
        basis[r.basis] += 1
        if r.generic:
            generic += 1
        if r.penalty > 0:
            daw2 += 1
            pen += r.penalty
    return (f"{ph.name} pricing: allowed {money(allowed)} (ingredient {money(ing)}, fees {money(fees)}); "
            f"awp {basis['awp']}, mac {basis['mac']}, u&c {basis['uc']}; generic rate {pct1(generic, paid)}; "
            f"DAW 2 penalties {money(pen)} on {plural(daw2, 'fill')}")


def drugs_report(ph, drugs, claims, adj):
    names = {d.ndc: d.name for d in drugs}
    total = {}
    fills = {}
    for c, r in zip(claims, adj):
        if r.reject < 0:
            total[c.ndc] = total.get(c.ndc, 0) + r.allowed
            fills[c.ndc] = fills.get(c.ndc, 0) + 1
    ranked = sorted(total, key=lambda k: (-total[k], names[k]))
    top = ", ".join(f"{names[k]} {money(total[k])} ({fills[k]})" for k in ranked[:3])
    return f"{ph.name} top drugs: {top}"


def controlled_report(ph, drugs, claims, adj):
    sched = {d.ndc: d.schedule for d in drugs}
    c2 = c4 = units = overrides = 0
    by_dea = {}
    by_day = [0] * MONTH_LEN
    for c, r in zip(claims, adj):
        if r.reject >= 0:
            continue
        by_day[c.day] += 1
        if r.override:
            overrides += 1
        s = sched[c.ndc]
        if s == 0:
            continue
        if s == 2:
            c2 += 1
        else:
            c4 += 1
        units += c.qty
        by_dea[c.dea] = by_dea.get(c.dea, 0) + 1
    top = "none"
    if by_dea:
        k = sorted(by_dea, key=lambda d: (-by_dea[d], d))[0]
        top = f"{k} with {by_dea[k]}"
    busiest = 0
    for d in range(MONTH_LEN):
        if by_day[d] > by_day[busiest]:
            busiest = d
    return (f"{ph.name} controlled: {c2} CII and {c4} CIV fills paid, {group3(units)} units; top prescriber {top}; "
            f"{plural(overrides, 'DUR override')}; busiest day {day_name(busiest)} with {by_day[busiest]} paid")


def reject_samples(ph, drugs, claims, adj):
    names = {d.ndc: d.name for d in drugs}
    out = []
    for k in range(len(REJECTS)):
        for c, r in zip(claims, adj):
            if r.reject == k:
                out.append(f"  {REJECTS[k]} {REJECT_NAMES[k]}: {c.rx} {c.member} {names[c.ndc]} {date_of(c.day)[5:]}")
                break
    return out


# ------------------------------------------------------------ accumulators

class Accumulators:
    def __init__(self, members):
        self.plan = {m.mid: m.plan for m in members}
        self.ded = {m.mid: m.ytd_ded for m in members}
        self.oop = {m.mid: m.ytd_oop for m in members}
        self.share = {}
        self.log = ["rx,member,allowed,member_share,penalty,plan_paid,ytd_deductible,ytd_oop"]
        # (member, what happened), and the pharmacies each member filled at
        self.events = []
        self.where = {}
        self.met = 0
        self.maxed = 0
        self.member_total = 0
        self.plan_total = 0


def post_claims(acc, ph, drugs, claims, adj):
    tiers = {d.ndc: d.tier for d in drugs}
    n = mem_total = pen_total = plan_total = met = maxed = 0
    for c, r in zip(claims, adj):
        if r.reject >= 0:
            continue
        n += 1
        p = PLANS[acc.plan[c.member]]
        ded_left = p.ded - acc.ded[c.member]
        oop_left = p.oop - acc.oop[c.member]
        dpart = min(r.allowed, ded_left) if ded_left > 0 else 0
        rest = r.allowed - dpart
        tier = tiers[c.ndc]
        if tier < 4:
            cs = min(p.copay[tier - 1], rest)
        else:
            cs = rest * p.coins // 100
        mem = min(dpart + cs, oop_left)
        acc.ded[c.member] += min(dpart, mem)
        acc.oop[c.member] += mem
        plan_paid = r.allowed - mem
        if ded_left > 0 and acc.ded[c.member] == p.ded:
            met += 1
            acc.events.append((c.member, f"{c.member} met the {money(p.ded)} deductible at {ph.name} on {date_of(c.day)[5:]}"))
        if oop_left > 0 and acc.oop[c.member] == p.oop:
            maxed += 1
            acc.events.append((c.member, f"{c.member} reached the {money(p.oop)} out-of-pocket maximum at {ph.name} on {date_of(c.day)[5:]}"))
        acc.share[c.member] = acc.share.get(c.member, 0) + mem + r.penalty
        at = acc.where.setdefault(c.member, [])
        if ph.name not in at:
            at.append(ph.name)
        mem_total += mem
        pen_total += r.penalty
        plan_total += plan_paid
        acc.log.append(",".join([c.rx, c.member, dollars(r.allowed), dollars(mem), dollars(r.penalty), dollars(plan_paid),
                                 dollars(acc.ded[c.member]), dollars(acc.oop[c.member])]))
    acc.met += met
    acc.maxed += maxed
    acc.member_total += mem_total + pen_total
    acc.plan_total += plan_total
    return (f"{ph.name} posted: {n} paid claims, member share {money(mem_total)} plus {money(pen_total)} in penalties, "
            f"plan paid {money(plan_total)}; {plural(met, 'deductible')} met, {plural(maxed, 'out-of-pocket maximum')} reached")


def accumulator_summary(acc):
    # Only the members who filled at more than one pharmacy: the order the
    # pharmacies post in decides where their thresholds fall.
    out = [what for mid, what in acc.events if len(acc.where[mid]) > 1]
    ranked = sorted(acc.share, key=lambda k: (-acc.share[k], k))
    top = ", ".join(f"{k} {money(acc.share[k])}" for k in ranked[:3])
    shared = sum(1 for mid in acc.where if len(acc.where[mid]) > 1)
    out.append(f"members: {len(acc.share)} with paid claims, {shared} at more than one pharmacy, {acc.met} met the deductible and {acc.maxed} reached the "
               f"out-of-pocket maximum this month")
    out.append(f"month: members paid {money(acc.member_total)}, the plan paid {money(acc.plan_total)}; most paid {top}")
    return out


# ------------------------------------------------------------ the job

def run():
    ashford = Pharmacy("ashford", "A", 101, 150, 1700, 85, 220)
    brookline = Pharmacy("brookline", "B", 202, 175, 1850, 75, 170)
    cedar = Pharmacy("cedar", "C", 303, 200, 1600, 90, 190)
    dalton = Pharmacy("dalton", "D", 404, 125, 2000, 70, 260)

    drugs = build_formulary()
    members = build_members(drugs)
    prescribers = build_prescribers()

    c1 = build_claims(ashford, drugs, members, prescribers)
    c2 = build_claims(brookline, drugs, members, prescribers)
    c3 = build_claims(cedar, drugs, members, prescribers)
    c4 = build_claims(dalton, drugs, members, prescribers)

    write_text("formulary.csv", encode_formulary(drugs))
    write_text("members.csv", encode_members(members))
    write_text("ashford_claims.csv", encode_claims(c1))
    write_text("brookline_claims.csv", encode_claims(c2))
    write_text("cedar_claims.csv", encode_claims(c3))
    write_text("dalton_claims.csv", encode_claims(c4))

    whole = read_text("ashford_claims.csv")
    bad_dea = whole.split("\n")[40].split(",")[7]
    bad_dea = bad_dea[:8] + str((int(bad_dea[8]) + 1) % 10)
    write_text("ashford_claims.bad", damage(whole, 40, 7, bad_dea))

    tf = read_text("formulary.csv")
    tm = read_text("members.csv")
    t1 = read_text("ashford_claims.csv")
    t2 = read_text("brookline_claims.csv")
    t3 = read_text("cedar_claims.csv")
    t4 = read_text("dalton_claims.csv")
    for name, t in [("formulary", tf), ("members", tm), ("ashford", t1), ("brookline", t2), ("cedar", t3), ("dalton", t4)]:
        print(f"{name}: {group3(len(t.split(chr(10))) - 2)} rows, {group3(len(t))} bytes, fnv {text_sum(t):08x}")

    gd = parse_formulary("formulary.csv", tf)
    gm = parse_members("members.csv", tm)
    g1 = parse_claims("ashford_claims.csv", ashford, gd, gm, t1)
    g2 = parse_claims("brookline_claims.csv", brookline, gd, gm, t2)
    g3 = parse_claims("cedar_claims.csv", cedar, gd, gm, t3)
    g4 = parse_claims("dalton_claims.csv", dalton, gd, gm, t4)
    try:
        parse_claims("ashford_claims.bad", ashford, gd, gm, read_text("ashford_claims.bad"))
        print("ashford_claims.bad: parsed")
    except BatchError as e:
        print(e)
    try:
        parse_claims("brookline_claims.csv", cedar, gd, gm, t2)
        print("brookline_claims.csv: parsed")
    except BatchError as e:
        print(e)
    same = ([d.key() for d in gd] == [d.key() for d in drugs] and [m.key() for m in gm] == [m.key() for m in members]
            and all([c.key() for c in a] == [c.key() for c in b] for a, b in [(g1, c1), (g2, c2), (g3, c3), (g4, c4)]))
    print(f"parsed files match the built ones: {'true' if same else 'false'}")

    a1 = adjudicate(ashford, gd, gm, g1)
    a2 = adjudicate(brookline, gd, gm, g2)
    a3 = adjudicate(cedar, gd, gm, g3)
    a4 = adjudicate(dalton, gd, gm, g4)

    for ph, g, a in [(ashford, g1, a1), (brookline, g2, a2), (cedar, g3, a3), (dalton, g4, a4)]:
        print(claims_report(ph, a))
        for line in reject_samples(ph, gd, g, a):
            print(line)
    for ph, a in [(ashford, a1), (brookline, a2), (cedar, a3), (dalton, a4)]:
        print(pricing_report(ph, a))
    for ph, g, a in [(ashford, g1, a1), (brookline, g2, a2), (cedar, g3, a3), (dalton, g4, a4)]:
        print(drugs_report(ph, gd, g, a))
    for ph, g, a in [(ashford, g1, a1), (brookline, g2, a2), (cedar, g3, a3), (dalton, g4, a4)]:
        print(controlled_report(ph, gd, g, a))

    acc = Accumulators(gm)
    order = [(ashford, g1, a1), (brookline, g2, a2), (cedar, g3, a3), (dalton, g4, a4)]
    if os.environ.get("RX_REVERSE") == "1":
        order.reverse()
    for ph, g, a in order:
        print(post_claims(acc, ph, gd, g, a))
    for line in accumulator_summary(acc):
        print(line)
    write_text("accumulators.log", "\n".join(acc.log) + "\n")
    back = read_text("accumulators.log")
    print(f"accumulators.log: {group3(len(back.split(chr(10))) - 2)} entries, {group3(len(back))} bytes, fnv {text_sum(back):08x}")

    for p in ["formulary.csv", "members.csv", "ashford_claims.csv", "brookline_claims.csv", "cedar_claims.csv",
              "dalton_claims.csv", "ashford_claims.bad", "accumulators.log"]:
        os.remove(p)


def main():
    try:
        run()
    except BatchError as e:
        print(f"rx_adjudicate failed: {e}")


main()
