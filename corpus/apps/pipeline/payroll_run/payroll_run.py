"""Python mirror of payroll_run.kara: the same generator, parsers, pay rules
and reports, statement for statement, so its output is the expected one.

Run: python3 -I -B payroll_run.py > expected.out"""
import os

MASK32 = 4294967295
EMPLOYEES = 1200
DAYS = 14
WEEK_LIMIT = 2400
SS_BASE = 16860000
ADDL_MEDICARE_AT = 20000000
START_BALANCE = 1150000000
FIRST_CHECK = 100001

SYLLABLES = ["ka", "ri", "to", "men", "sa", "lo", "vi", "dan", "el", "mor", "ra", "ne", "jo", "lin", "ta", "bo"]
PLANS = ["none", "basic", "family"]
PREMIUMS = [0, 4850, 13200]
K401 = [0, 0, 3, 4, 5, 6, 8, 10]

FED_SINGLE = [(0, 0), (600000, 1000), (1760000, 1200), (5315000, 2200), (10652500, 2400),
              (19795000, 3200), (24972500, 3500), (61535000, 3700)]
FED_MARRIED = [(0, 0), (1630000, 1000), (3950000, 1200), (11060000, 2200), (21735000, 2400),
               (40020000, 3200), (50375000, 3500), (74750000, 3700)]
CA_BRACKETS = [(0, 100), (1075600, 200), (2549900, 400), (4024500, 600), (5586600, 800),
               (7060600, 930), (36065900, 1030)]


class PayError(Exception):
    def __init__(self, what):
        self.what = what


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


def money(cents):
    whole = str(cents // 100)
    out = ""
    n = len(whole)
    for i in range(n):
        if i > 0 and (n - i) % 3 == 0:
            out += ","
        out += whole[i]
    return "$" + out + "." + str(cents % 100).rjust(2, "0")


def hours(minutes):
    return str(minutes // 60) + "." + str((minutes % 60) * 10 // 60) + "h"


def clock(m):
    if m < 0:
        return "--:--"
    return str(m // 60).rjust(2, "0") + ":" + str(m % 60).rjust(2, "0")


def cap(s):
    return s[0].upper() + s[1:]


# ------------------------------------------------------------ the data

class Employee:
    def __init__(self, eid, name, kind, rate, filing, k401, plan, ytd_ss):
        self.eid = eid
        self.name = name
        self.kind = kind
        self.rate = rate
        self.filing = filing
        self.k401 = k401
        self.plan = plan
        self.ytd_ss = ytd_ss


class Punch:
    def __init__(self, eid, day, t_in, t_out, brk):
        self.eid = eid
        self.day = day
        self.t_in = t_in
        self.t_out = t_out
        self.brk = brk


class Site:
    def __init__(self, name, state, seed):
        self.name = name
        self.state = state
        self.seed = seed
        self.code = name[:3].upper()


def build_roster(site):
    s = site.seed
    out = []
    for i in range(EMPLOYEES):
        eid = site.code + str(i + 1).rjust(4, "0")
        first = cap(SYLLABLES[mix(s, i, 1) % 16] + SYLLABLES[mix(s, i, 2) % 16])
        last = cap(SYLLABLES[mix(s, i, 3) % 16] + SYLLABLES[mix(s, i, 4) % 16] + SYLLABLES[mix(s, i, 5) % 16])
        salaried = mix(s, i, 6) % 10 < 3
        if salaried:
            rate = (52000 + (mix(s, i, 7) % 1881) * 100) * 100
        else:
            rate = 1800 + mix(s, i, 7) % 4701
        filing = "M" if mix(s, i, 8) % 5 < 2 else "S"
        k401 = K401[mix(s, i, 9) % 8]
        plan = PLANS[mix(s, i, 10) % 3]
        h = mix(s, i, 11)
        if h % 10 == 0:
            ytd = 16000000 + h % 4000000
        else:
            ytd = h % 9000000
        out.append(Employee(eid, first + " " + last, "salaried" if salaried else "hourly",
                            rate, filing, k401, plan, ytd))
    return out


def build_times(site, roster):
    s = site.seed
    out = []
    for i in range(len(roster)):
        e = roster[i]
        if e.kind != "hourly":
            continue
        for d in range(DAYS):
            h = mix(s, i * 16 + d, 12)
            if h % 7 >= 5:
                continue
            t_in = 360 + (h >> 4) % 240
            length = 240 + (h >> 12) % 421
            if (h >> 22) % 40 == 0:
                length += 180
            t_out = t_in + length
            brk = 30 if length > 360 else 0
            k = mix(s, i * 16 + d, 13) % 200
            if k == 0:
                out.append(Punch(e.eid, d, t_in, -1, brk))
            else:
                out.append(Punch(e.eid, d, t_in, t_out, brk))
                if k == 1:
                    out.append(Punch(e.eid, d, t_in + 60, t_out, 0))
    if s % 3 == 2:
        out.append(Punch(site.code + "9999", 3, 480, 960, 30))
    return out


def encode_roster(roster):
    lines = ["id,name,kind,rate,filing,k401,plan,ytd_ss"]
    for e in roster:
        lines.append(",".join([e.eid, e.name, e.kind, str(e.rate), e.filing, str(e.k401), e.plan, str(e.ytd_ss)]))
    return "\n".join(lines) + "\n"


def encode_times(punches):
    lines = ["id,day,in,out,break"]
    for p in punches:
        lines.append(",".join([p.eid, str(p.day), clock(p.t_in), clock(p.t_out), str(p.brk)]))
    return "\n".join(lines) + "\n"


def read_text(path):
    try:
        with open(path) as f:
            return f.read()
    except OSError:
        raise PayError("could not read " + path)


def damage(text, n):
    lines = text.split("\n")
    fields = lines[n + 1].split(",")
    fields[2] = "25:61"
    lines[n + 1] = ",".join(fields)
    return "\n".join(lines)


# ------------------------------------------------------------ the parsers

def is_digits(s):
    if len(s) == 0:
        return False
    for c in s:
        if c < "0" or c > "9":
            return False
    return True


def is_upper(c):
    return "A" <= c <= "Z"


def parse_clock(s):
    if s == "--:--":
        return -1
    if len(s) != 5 or s[2] != ":" or not is_digits(s[0:2]) or not is_digits(s[3:5]):
        return -2
    h = int(s[0:2])
    m = int(s[3:5])
    if h > 23 or m > 59:
        return -2
    return h * 60 + m


def check_header(path, lines, header):
    if len(lines) == 0 or lines[0] != header:
        raise PayError(path + " line 1: bad header")


def parse_roster(path, text):
    lines = text.split("\n")
    check_header(path, lines, "id,name,kind,rate,filing,k401,plan,ytd_ss")
    out = []
    for n in range(1, len(lines)):
        line = lines[n]
        if line == "":
            continue
        where = path + " line " + str(n + 1) + ": "
        f = line.split(",")
        if len(f) != 8:
            raise PayError(where + str(len(f)) + " fields")
        eid = f[0]
        if len(eid) != 7 or not (is_upper(eid[0]) and is_upper(eid[1]) and is_upper(eid[2])) or not is_digits(eid[3:7]):
            raise PayError(where + "bad id '" + eid + "'")
        if f[2] != "hourly" and f[2] != "salaried":
            raise PayError(where + "bad kind '" + f[2] + "'")
        if not is_digits(f[3]) or not is_digits(f[5]) or not is_digits(f[7]):
            raise PayError(where + "bad number")
        if f[4] != "S" and f[4] != "M":
            raise PayError(where + "bad filing status '" + f[4] + "'")
        k401 = int(f[5])
        if k401 > 15:
            raise PayError(where + "401(k) rate " + str(k401) + "% over 15%")
        if f[6] not in PLANS:
            raise PayError(where + "bad plan '" + f[6] + "'")
        out.append(Employee(eid, f[1], f[2], int(f[3]), f[4], k401, f[6], int(f[7])))
    return out


def parse_times(path, text):
    lines = text.split("\n")
    check_header(path, lines, "id,day,in,out,break")
    out = []
    for n in range(1, len(lines)):
        line = lines[n]
        if line == "":
            continue
        where = path + " line " + str(n + 1) + ": "
        f = line.split(",")
        if len(f) != 5:
            raise PayError(where + str(len(f)) + " fields")
        if not is_digits(f[1]) or not is_digits(f[4]):
            raise PayError(where + "bad number")
        day = int(f[1])
        if day >= DAYS:
            raise PayError(where + "day " + str(day) + " outside the period")
        t_in = parse_clock(f[2])
        if t_in < 0:
            raise PayError(where + "bad time '" + f[2] + "'")
        t_out = parse_clock(f[3])
        if t_out == -2:
            raise PayError(where + "bad time '" + f[3] + "'")
        if t_out >= 0 and t_out <= t_in:
            raise PayError(where + "punch-out before punch-in")
        out.append(Punch(f[0], day, t_in, t_out, int(f[4])))
    return out


def same_roster(a, b):
    if len(a) != len(b):
        return False
    for x, y in zip(a, b):
        if (x.eid, x.name, x.kind, x.rate, x.filing, x.k401, x.plan, x.ytd_ss) != \
           (y.eid, y.name, y.kind, y.rate, y.filing, y.k401, y.plan, y.ytd_ss):
            return False
    return True


def same_times(a, b):
    if len(a) != len(b):
        return False
    for x, y in zip(a, b):
        if (x.eid, x.day, x.t_in, x.t_out, x.brk) != (y.eid, y.day, y.t_in, y.t_out, y.brk):
            return False
    return True


# ------------------------------------------------------------ hours

class Hours:
    def __init__(self, eid):
        self.eid = eid
        self.reg = 0
        self.ot = 0
        self.dt = 0
        self.days = 0


class Anomaly:
    def __init__(self, eid, day, what):
        self.eid = eid
        self.day = day
        self.what = what


class Timecard:
    def __init__(self):
        self.hours = []
        self.anomalies = []


def find_employee(roster, eid):
    lo = 0
    hi = len(roster)
    while lo < hi:
        mid = (lo + hi) // 2
        if roster[mid].eid < eid:
            lo = mid + 1
        else:
            hi = mid
    if lo < len(roster) and roster[lo].eid == eid:
        return lo
    return -1


def compute_hours(site, roster, punches):
    card = Timecard()
    minutes = [[0] * DAYS for _ in roster]
    last_out = [[-1] * DAYS for _ in roster]
    for p in punches:
        k = find_employee(roster, p.eid)
        if k < 0:
            card.anomalies.append(Anomaly(p.eid, p.day, "unknown employee"))
            continue
        if p.t_out < 0:
            card.anomalies.append(Anomaly(p.eid, p.day, "missing punch-out"))
            continue
        if last_out[k][p.day] > p.t_in:
            card.anomalies.append(Anomaly(p.eid, p.day, "overlapping punches"))
            continue
        last_out[k][p.day] = p.t_out
        minutes[k][p.day] += p.t_out - p.t_in - p.brk
    daily = site.state == "CA"
    for k in range(len(roster)):
        e = roster[k]
        h = Hours(e.eid)
        if e.kind == "hourly":
            for week in range(2):
                week_reg = 0
                for d in range(week * 7, week * 7 + 7):
                    m = minutes[k][d]
                    if m > 0:
                        h.days += 1
                    if daily:
                        reg = min(m, 480)
                        ot = min(max(m - 480, 0), 240)
                        dt = max(m - 720, 0)
                    else:
                        reg = m
                        ot = 0
                        dt = 0
                    week_reg += reg
                    h.ot += ot
                    h.dt += dt
                if week_reg > WEEK_LIMIT:
                    h.ot += week_reg - WEEK_LIMIT
                    week_reg = WEEK_LIMIT
                h.reg += week_reg
        card.hours.append(h)
    return card


# ------------------------------------------------------------ pay

class Pay:
    def __init__(self, eid):
        self.eid = eid
        self.gross = 0
        self.k401 = 0
        self.health = 0
        self.taxable = 0
        self.fed = 0
        self.ss = 0
        self.medicare = 0
        self.state = 0
        self.net = 0
        self.capped = False
        self.employer = 0


def bracket_tax(annual, table):
    total = 0
    for j in range(len(table)):
        lo, bp = table[j]
        if annual <= lo:
            break
        hi = table[j + 1][0] if j + 1 < len(table) else annual
        top = min(annual, hi)
        total += (top - lo) * bp
    return (total + 5000) // 10000


def per_period(annual_tax):
    return (annual_tax + 13) // 26


def state_tax(state, taxable):
    if state == "CA":
        return per_period(bracket_tax(taxable * 26, CA_BRACKETS))
    if state == "CO":
        return (taxable * 440 + 5000) // 10000
    if state == "MA":
        return (taxable * 500 + 5000) // 10000
    return 0


def run_pay(site, roster, card):
    out = []
    for k in range(len(roster)):
        e = roster[k]
        h = card.hours[k]
        p = Pay(e.eid)
        if e.kind == "hourly":
            p.gross = (h.reg * e.rate * 2 + h.ot * e.rate * 3 + h.dt * e.rate * 4 + 60) // 120
        else:
            p.gross = e.rate // 26
        p.k401 = (p.gross * e.k401 + 50) // 100
        p.health = min(PREMIUMS[PLANS.index(e.plan)], p.gross - p.k401)
        p.taxable = p.gross - p.k401 - p.health
        table = FED_MARRIED if e.filing == "M" else FED_SINGLE
        p.fed = per_period(bracket_tax(p.taxable * 26, table))
        wages = p.gross - p.health
        room = max(SS_BASE - e.ytd_ss, 0)
        taxed = min(wages, room)
        p.capped = taxed < wages
        p.ss = (taxed * 62 + 500) // 1000
        over = max(e.ytd_ss + wages - max(ADDL_MEDICARE_AT, e.ytd_ss), 0)
        p.medicare = (wages * 145 + 5000) // 10000 + (over * 90 + 5000) // 10000
        p.state = state_tax(site.state, p.taxable)
        p.net = p.gross - p.k401 - p.health - p.fed - p.ss - p.medicare - p.state
        p.employer = p.gross + p.ss + (wages * 145 + 5000) // 10000
        out.append(p)
    return out


# ------------------------------------------------------------ reports

def totals_report(site, roster, pays):
    hourly = sum(1 for e in roster if e.kind == "hourly")
    gross = sum(p.gross for p in pays)
    net = sum(p.net for p in pays)
    taxes = sum(p.fed + p.ss + p.medicare + p.state for p in pays)
    cost = sum(p.employer for p in pays)
    lines = [site.name + " totals: " + str(len(roster)) + " employees (" + str(hourly) + " hourly, "
             + str(len(roster) - hourly) + " salaried), gross " + money(gross) + ", net " + money(net)
             + ", taxes " + money(taxes) + ", employer cost " + money(cost)]
    for k in [0, 1, len(pays) - 1]:
        e = roster[k]
        p = pays[k]
        lines.append("  " + e.eid + " " + e.name + " (" + e.kind + ", " + e.filing + "): gross " + money(p.gross)
                     + " - 401k " + money(p.k401) + " - health " + money(p.health) + " - fed " + money(p.fed)
                     + " - ss " + money(p.ss) + " - medicare " + money(p.medicare) + " - state " + money(p.state)
                     + " = " + money(p.net))
    return "\n".join(lines)


def overtime_report(site, roster, card):
    ot = sum(h.ot for h in card.hours)
    dt = sum(h.dt for h in card.hours)
    ranked = sorted(range(len(roster)), key=lambda k: (-(card.hours[k].ot + card.hours[k].dt), roster[k].eid))
    tops = []
    for k in ranked[:3]:
        h = card.hours[k]
        tops.append(roster[k].eid + " " + hours(h.ot + h.dt) + " over " + str(h.days) + " days")
    return site.name + " overtime: " + hours(ot) + " at 1.5x, " + hours(dt) + " at 2x; most: " + "; ".join(tops)


def anomaly_report(site, card):
    missing = sum(1 for a in card.anomalies if a.what == "missing punch-out")
    overlap = sum(1 for a in card.anomalies if a.what == "overlapping punches")
    unknown = sum(1 for a in card.anomalies if a.what == "unknown employee")
    first = [a.eid + " day " + str(a.day) + " " + a.what for a in card.anomalies[:3]]
    return (site.name + " anomalies: " + str(len(card.anomalies)) + " (" + str(missing) + " missing punch-outs, "
            + str(overlap) + " overlapping, " + str(unknown) + " with an unknown id); first: " + "; ".join(first))


def tax_report(site, pays):
    fed = sum(p.fed for p in pays)
    ss = sum(p.ss for p in pays)
    med = sum(p.medicare for p in pays)
    st = sum(p.state for p in pays)
    taxable = sum(p.taxable for p in pays)
    capped = sum(1 for p in pays if p.capped)
    bp = fed * 10000 // taxable
    return (site.name + " taxes: federal " + money(fed) + ", social security " + money(ss) + ", medicare "
            + money(med) + ", state (" + site.state + ") " + money(st) + "; " + str(capped)
            + " past the wage base; federal " + str(bp // 100) + "." + str(bp % 100).rjust(2, "0")
            + "% of taxable")


def net_report(site, pays):
    nets = sorted(p.net for p in pays)
    n = len(nets)
    median = nets[(n + 1) // 2 - 1]
    p90 = nets[(n * 9 + 9) // 10 - 1]
    buckets = [0] * 10
    for v in nets:
        b = v // 100000
        if b > 9:
            b = 9
        buckets[b] += 1
    parts = []
    for b in range(10):
        label = str(b) + "k" if b < 9 else "9k+"
        parts.append(label + ":" + str(buckets[b]))
    return site.name + " net pay: median " + money(median) + ", p90 " + money(p90) + ", " + " ".join(parts)


# ------------------------------------------------------------ the ledger

class Ledger:
    def __init__(self):
        self.balance = START_BALANCE
        self.next_check = FIRST_CHECK
        self.trace = 0
        self.held = 0
        self.held_amount = 0
        self.ach = ""


def post(gl, site, roster, pays):
    paid = 0
    held = 0
    held_amount = 0
    first = gl.next_check
    for k in range(len(pays)):
        p = pays[k]
        if p.net > gl.balance:
            held += 1
            held_amount += p.net
            continue
        gl.balance -= p.net
        gl.trace += 1
        check = gl.next_check
        gl.next_check += 1
        paid += 1
        gl.ach += ("6" + site.code + str(check) + str(p.net).rjust(10, "0") + p.eid
                   + roster[k].name.ljust(22)[:22] + str(gl.trace).rjust(7, "0") + "\n")
    gl.held += held
    gl.held_amount += held_amount
    last = gl.next_check - 1
    checks = "checks " + str(first) + "-" + str(last) if paid > 0 else "no checks"
    return (site.name + " posted: " + str(paid) + " payments, " + checks + ", " + str(held) + " held (" + money(held_amount) + "), balance " + money(gl.balance))


# ------------------------------------------------------------ the job

def run():
    den = Site("denver", "CO", 11)
    aus = Site("austin", "TX", 23)
    oak = Site("oakland", "CA", 37)
    bos = Site("boston", "MA", 41)

    r1 = build_roster(den)
    r2 = build_roster(aus)
    r3 = build_roster(oak)
    r4 = build_roster(bos)

    s1 = build_times(den, r1)
    s2 = build_times(aus, r2)
    s3 = build_times(oak, r3)
    s4 = build_times(bos, r4)

    names = ["denver", "austin", "oakland", "boston"]
    texts = [encode_roster(r1), encode_roster(r2), encode_roster(r3), encode_roster(r4),
             encode_times(s1), encode_times(s2), encode_times(s3), encode_times(s4)]
    paths = [n + ".roster" for n in names] + [n + ".times" for n in names]
    for path, text in zip(paths, texts):
        with open(path, "w") as f:
            f.write(text)

    whole = read_text("oakland.times")
    with open("oakland.bad", "w") as f:
        f.write(damage(whole, 300))

    got = [read_text(p) for p in paths]
    for path, t in zip(paths, got):
        print(path + ": " + str(len(t)) + " bytes, fnv " + format(text_sum(t), "08x"))

    g1 = parse_roster("denver.roster", got[0])
    g2 = parse_roster("austin.roster", got[1])
    g3 = parse_roster("oakland.roster", got[2])
    g4 = parse_roster("boston.roster", got[3])
    t1 = parse_times("denver.times", got[4])
    t2 = parse_times("austin.times", got[5])
    t3 = parse_times("oakland.times", got[6])
    t4 = parse_times("boston.times", got[7])
    try:
        parse_times("oakland.bad", read_text("oakland.bad"))
        print("oakland.bad: parsed")
    except PayError as e:
        print(e.what)
    try:
        parse_roster("boston.times", got[7])
        print("boston.times as a roster: parsed")
    except PayError as e:
        print(e.what)
    same = same_roster(g1, r1) and same_roster(g2, r2) and same_roster(g3, r3) and same_roster(g4, r4)
    print("parsed rosters match the built ones: " + str(same).lower())
    same_t = same_times(t1, s1) and same_times(t2, s2) and same_times(t3, s3) and same_times(t4, s4)
    print("parsed timesheets match the built ones: " + str(same_t).lower())

    c1 = compute_hours(den, g1, t1)
    c2 = compute_hours(aus, g2, t2)
    c3 = compute_hours(oak, g3, t3)
    c4 = compute_hours(bos, g4, t4)

    p1 = run_pay(den, g1, c1)
    p2 = run_pay(aus, g2, c2)
    p3 = run_pay(oak, g3, c3)
    p4 = run_pay(bos, g4, c4)

    reports = [totals_report(den, g1, p1), totals_report(aus, g2, p2), totals_report(oak, g3, p3),
               totals_report(bos, g4, p4),
               overtime_report(den, g1, c1), overtime_report(aus, g2, c2), overtime_report(oak, g3, c3),
               overtime_report(bos, g4, c4),
               anomaly_report(den, c1), anomaly_report(aus, c2), anomaly_report(oak, c3), anomaly_report(bos, c4),
               tax_report(den, p1), tax_report(aus, p2), tax_report(oak, p3), tax_report(bos, p4),
               net_report(den, p1), net_report(aus, p2), net_report(oak, p3), net_report(bos, p4)]
    for line in reports:
        print(line)

    gl = Ledger()
    print(post(gl, den, g1, p1))
    print(post(gl, aus, g2, p2))
    print(post(gl, oak, g3, p3))
    print(post(gl, bos, g4, p4))
    with open("payments.ach", "w") as f:
        f.write(gl.ach)
    back = read_text("payments.ach")
    rows = len(back.split("\n")) - 1
    print("payments.ach: " + str(rows) + " entries, " + str(gl.held) + " held (" + money(gl.held_amount) + "), "
          + str(len(back)) + " bytes, fnv " + format(text_sum(back), "08x"))

    for p in paths + ["oakland.bad", "payments.ach"]:
        os.remove(p)


def main():
    try:
        run()
    except PayError as e:
        print("payroll_run failed: " + e.what)


main()
