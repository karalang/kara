"""Python mirror of risk_batch.kara: the same positions, parsers, scenarios,
pricing, VaR and limit book, statement for statement, so its output is the
expected one.

Run: python3 -I -B risk_batch.py > expected.out"""
import os

MASK32 = 4294967295
POSITIONS = 240
DAYS = 250
ISSUERS = 16
FIRM_CAP = 1200000
HEADER = "id,book,kind,factor,qty,delta,gamma,issuer"
FACTORS = ["r2y", "r5y", "r10y", "r30y", "spx", "sx5e", "nky", "eurusd", "usdjpy", "gbpusd", "ig", "hy"]
BETA = [-30, -40, -45, -40, 90, 85, 70, 20, -35, 25, -60, -80]
IDIO = [60, 55, 50, 55, 45, 50, 70, 80, 75, 70, 50, 70]
GAMMA_KINDS = ["call", "put", "option", "bond"]
STRESS_NAMES = ["crash", "taper", "rates+200"]
STRESSES = [
    [-80, -70, -60, -50, -900, -950, -800, -150, -300, 120, 180, 450],
    [60, 90, 110, 80, -250, -200, -150, -80, 150, -60, 40, 120],
    [200, 200, 200, 200, -120, -100, -90, 50, 100, 30, 20, 60],
]


class RiskError(Exception):
    def __init__(self, what):
        self.what = what


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


def money(x):
    s = "-" if x < 0 else ""
    a = -x if x < 0 else x
    if a >= 1000000:
        return f"{s}{a // 1000000}.{(a % 1000000) // 100000}m"
    return f"{s}{a // 1000}.{(a % 1000) // 100}k"


class Desk:
    def __init__(self, name, seed, books, kinds, lo, hi, credit, limit):
        self.name = name
        self.seed = seed
        self.books = books
        self.kinds = kinds
        self.lo = lo
        self.hi = hi
        self.credit = credit
        self.limit = limit


class Pos:
    def __init__(self, pid, book, kind, factor, qty, delta, gamma, issuer):
        self.pid = pid
        self.book = book
        self.kind = kind
        self.factor = factor
        self.qty = qty
        self.delta = delta
        self.gamma = gamma
        self.issuer = issuer


# ------------------------------------------------------------ the data

def build_positions(desk):
    out = []
    nf = desk.hi - desk.lo
    for i in range(POSITIONS):
        h = mix(desk.seed, i, 1)
        h2 = mix(desk.seed, i, 2)
        h3 = mix(desk.seed, i, 3)
        book = desk.books[h % len(desk.books)]
        kind = desk.kinds[(h >> 4) % len(desk.kinds)]
        factor = desk.lo + (h >> 8) % nf
        size = (1 + h2 % 50) * 100000
        sign = -1 if (h2 >> 10) % 3 == 0 else 1
        direction = -1 if kind == "bond" or kind == "put" else 1
        sens = 1 + h3 % 90
        delta = direction * sign * tdiv(size * sens, 1000000)
        gamma = 0
        if kind == "bond" and not desk.credit:
            gamma = sign * (1 + (h3 >> 8) % 3)
        elif kind in GAMMA_KINDS and kind != "bond":
            gamma = sign * (1 + (h3 >> 8) % 20)
        issuer = "-"
        if desk.credit:
            issuer = f"ISS{(h3 >> 12) % ISSUERS:02}"
        out.append(Pos(f"{desk.name}-{i + 1:04}", book, kind, FACTORS[factor], sign * size, delta, gamma, issuer))
    return out


def encode_positions(ps):
    out = HEADER + "\n"
    for p in ps:
        out += f"{p.pid},{p.book},{p.kind},{p.factor},{p.qty},{p.delta},{p.gamma},{p.issuer}\n"
    return out


def read_text(path):
    try:
        with open(path, encoding="utf-8", newline="") as f:
            return f.read()
    except OSError:
        raise RiskError(f"could not read {path}")


def write_text(path, text):
    with open(path, "w", encoding="utf-8", newline="") as f:
        f.write(text)


# Replace the given field of the n-th position.
def damage(text, n, field, value):
    lines = text.split("\n")
    f = lines[n + 1].split(",")
    f[field] = value
    lines[n + 1] = ",".join(f)
    return "\n".join(lines)


# One scenario per day of a 250-day window: a common market move, an own
# move per factor, crisis days with three times the common move, and
# issuer defaults that are likelier on a crisis day.
class Scenarios:
    def __init__(self):
        self.shocks = []
        self.crisis = []
        self.defaults = []


def draw(h):
    return (h % 201) - 100 + ((h >> 8) % 201) - 100 + ((h >> 16) % 201) - 100


def build_scenarios():
    sc = Scenarios()
    for s in range(DAYS):
        crisis = mix(77, s, 5) % 29 == 0
        common = draw(mix(77, s, 6))
        if crisis:
            common = common * 3
        row = []
        for f in range(12):
            own = draw(mix(77, s * 12 + f, 7))
            row.append(tdiv(BETA[f] * common + IDIO[f] * own, 100))
        sc.shocks.append(row)
        sc.crisis.append(crisis)
        odds = 60 if crisis else 500
        d = []
        for i in range(ISSUERS):
            d.append(mix(77, s * ISSUERS + i, 9) % odds == 0)
        sc.defaults.append(d)
    return sc


# ------------------------------------------------------------ the parser

def is_digits(s):
    if len(s) == 0:
        return False
    for c in s:
        if c < "0" or c > "9":
            return False
    return True


def parse_int(s):
    t = s
    if t.startswith("-") or t.startswith("+"):
        t = t[1:]
    if not is_digits(t):
        return None
    v = int(s)
    if v < -(1 << 63) or v >= (1 << 63):
        return None
    return v


def factor_index(name):
    for i in range(12):
        if FACTORS[i] == name:
            return i
    return -1


def parse_positions(path, desk, text):
    lines = text.split("\n")
    if len(lines) == 0 or lines[0] != HEADER:
        raise RiskError(f"{path} line 1: bad header")
    prefix = f"{desk.name}-"
    out = []
    for n in range(1, len(lines)):
        line = lines[n]
        if line == "":
            continue
        where = f"{path} line {n + 1}: "
        f = line.split(",")
        if len(f) != 8:
            raise RiskError(f"{where}{len(f)} fields")
        if not f[0].startswith(prefix) or not is_digits(f[0][len(prefix):]):
            raise RiskError(f"{where}bad id '{f[0]}'")
        if f[1] not in desk.books:
            raise RiskError(f"{where}unknown book '{f[1]}'")
        if f[2] not in desk.kinds:
            raise RiskError(f"{where}unknown kind '{f[2]}'")
        fx = factor_index(f[3])
        if fx < desk.lo or fx >= desk.hi:
            raise RiskError(f"{where}factor '{f[3]}' is not on the {desk.name} desk")
        qty = parse_int(f[4])
        if qty is None:
            raise RiskError(f"{where}bad qty '{f[4]}'")
        if qty == 0:
            raise RiskError(f"{where}zero qty")
        delta = parse_int(f[5])
        if delta is None:
            raise RiskError(f"{where}bad delta '{f[5]}'")
        gamma = parse_int(f[6])
        if gamma is None:
            raise RiskError(f"{where}bad gamma '{f[6]}'")
        if gamma != 0 and f[2] not in GAMMA_KINDS:
            raise RiskError(f"{where}gamma on a linear {f[2]}")
        if desk.credit:
            if not f[7].startswith("ISS") or not is_digits(f[7][3:]) or int(f[7][3:]) >= ISSUERS:
                raise RiskError(f"{where}bad issuer '{f[7]}'")
        elif f[7] != "-":
            raise RiskError(f"{where}issuer '{f[7]}' off the credit desk")
        out.append(Pos(f[0], f[1], f[2], f[3], qty, delta, gamma, f[7]))
    return out


def same_positions(a, b):
    if len(a) != len(b):
        return False
    for i in range(len(a)):
        x = a[i]
        y = b[i]
        if (x.pid != y.pid or x.book != y.book or x.kind != y.kind or x.factor != y.factor or x.qty != y.qty
                or x.delta != y.delta or x.gamma != y.gamma or x.issuer != y.issuer):
            return False
    return True


# ------------------------------------------------------------ exposures and pricing

class Expo:
    def __init__(self, n):
        self.delta = [0] * n
        self.gamma = [0] * n
        self.gross = [0] * n
        self.jtd = [0] * ISSUERS


def jump(p):
    loss = tdiv(p.qty * 60, 100)
    return -loss if p.kind == "bond" else loss


def issuer_index(p):
    if p.issuer == "-":
        return -1
    return int(p.issuer[3:])


def exposures(desk, ps):
    e = Expo(desk.hi - desk.lo)
    for p in ps:
        k = factor_index(p.factor) - desk.lo
        e.delta[k] += p.delta
        e.gamma[k] += p.gamma
        e.gross[k] += -p.delta if p.delta < 0 else p.delta
        i = issuer_index(p)
        if i >= 0:
            e.jtd[i] += jump(p)
    return e


class Pnl:
    def __init__(self, nb):
        self.books = [[0] * DAYS for _ in range(nb)]
        self.total = [0] * DAYS
        self.default_days = 0


def book_index(desk, name):
    for i in range(len(desk.books)):
        if desk.books[i] == name:
            return i
    return -1


def move_pnl(p, x):
    return p.delta * x + tdiv(p.gamma * x * x, 200)


def price(desk, ps, sc):
    out = Pnl(len(desk.books))
    for s in range(DAYS):
        hit = False
        for p in ps:
            x = sc.shocks[s][factor_index(p.factor)]
            v = move_pnl(p, x)
            i = issuer_index(p)
            if i >= 0 and sc.defaults[s][i]:
                v += jump(p)
                hit = True
            b = book_index(desk, p.book)
            out.books[b][s] += v
            out.total[s] += v
        if hit:
            out.default_days += 1
    return out


class Risk:
    def __init__(self):
        self.var99 = 0
        self.es975 = 0
        self.worst_day = 0
        self.book_var = []


def var_of(v):
    s = sorted(v)
    return -s[2]


def es_of(v):
    s = sorted(v)
    total = 0
    for i in range(6):
        total += s[i]
    return -tdiv(total, 6)


def risk(pnl):
    r = Risk()
    r.var99 = var_of(pnl.total)
    r.es975 = es_of(pnl.total)
    worst = 0
    for s in range(DAYS):
        if pnl.total[s] < pnl.total[worst]:
            worst = s
    r.worst_day = worst
    for b in pnl.books:
        r.book_var.append(var_of(b))
    return r


# ------------------------------------------------------------ reports

def totals_report(desk, ps):
    longs = 0
    gross = 0
    for p in ps:
        if p.qty > 0:
            longs += 1
            gross += p.qty
        else:
            gross -= p.qty
    out = f"{desk.name}: {len(ps)} positions in {len(desk.books)} books, {longs} long, {len(ps) - longs} short, "
    out += f"gross {money(gross)};"
    for k in desk.kinds:
        n = 0
        for p in ps:
            if p.kind == k:
                n += 1
        out += f" {k} {n}"
    return out


def factor_report(desk, e):
    out = f"{desk.name} exposures:"
    for k in range(desk.hi - desk.lo):
        sep = "," if k > 0 else ""
        out += f"{sep} {FACTORS[desk.lo + k]} delta {money(e.delta[k])}/bp gamma {e.gamma[k]} gross {money(e.gross[k])}"
    if desk.credit:
        worst = 0
        for i in range(ISSUERS):
            if e.jtd[i] < e.jtd[worst]:
                worst = i
        out += f"; worst jump to default ISS{worst:02} {money(e.jtd[worst])}"
    return out


def book_report(desk, r):
    out = f"{desk.name} VaR99 {money(r.var99)}, ES97.5 {money(r.es975)}; books"
    total = 0
    for i in range(len(desk.books)):
        sep = "," if i > 0 else ""
        out += f"{sep} {desk.books[i]} {money(r.book_var[i])}"
        total += r.book_var[i]
    out += f"; diversification {money(total - r.var99)}"
    return out


def worst_report(desk, pnl, sc):
    days = list(range(DAYS))
    days.sort(key=lambda s: (pnl.total[s], s))
    out = f"{desk.name} worst days:"
    crisis = 0
    for j in range(5):
        s = days[j]
        sep = "," if j > 0 else ""
        mark = "*" if sc.crisis[s] else ""
        out += f"{sep} d{s + 1:03}{mark} {money(pnl.total[s])}"
        if sc.crisis[s]:
            crisis += 1
    out += f"; {crisis} of 5 on crisis days, {pnl.default_days} days with a default"
    return out


def stress_report(desk, ps):
    out = f"{desk.name} stress:"
    for j in range(3):
        v = 0
        for p in ps:
            v += move_pnl(p, STRESSES[j][factor_index(p.factor)])
        sep = "," if j > 0 else ""
        out += f"{sep} {STRESS_NAMES[j]} {money(v)}"
    return out


# ------------------------------------------------------------ the limit book

class LimitBook:
    def __init__(self, cap):
        self.cap = cap
        self.firm = [0] * DAYS
        self.firm_var = 0
        self.standalone = 0
        self.tickets = 0
        self.log = ""


def ticket(lb, what):
    lb.tickets += 1
    t = f"LB-{lb.tickets:04}"
    lb.log += f"{t} {what}\n"
    return t


def book(lb, desk, pnl, r):
    before = lb.firm_var
    for s in range(DAYS):
        lb.firm[s] += pnl.total[s]
    after = var_of(lb.firm)
    lb.firm_var = after
    lb.standalone += r.var99
    marginal = after - before
    out = f"{desk.name} booked: firm VaR {money(before)} -> {money(after)}, marginal {money(marginal)}"
    lb.log += f"book {desk.name} {r.var99} {marginal} {after}\n"
    if r.var99 > desk.limit:
        t = ticket(lb, f"{desk.name} desk VaR {r.var99} over its limit {desk.limit}")
        out += f"; {t} desk over its {money(desk.limit)} limit by {money(r.var99 - desk.limit)}"
    if after > lb.cap:
        if before > lb.cap:
            t = ticket(lb, f"{desk.name} keeps the firm over its cap at {after}")
            out += f"; {t} firm still over cap"
        else:
            t = ticket(lb, f"{desk.name} takes the firm over its cap {lb.cap} to {after}")
            out += f"; {t} firm over its {money(lb.cap)} cap by {money(after - lb.cap)}"
    elif marginal < 0:
        out += f"; diversifying, headroom {money(lb.cap - after)}"
    else:
        out += f"; headroom {money(lb.cap - after)}"
    return out


# ------------------------------------------------------------ the job

def run():
    sc = build_scenarios()
    crisis = sum(1 for c in sc.crisis if c)
    events = 0
    for d in sc.defaults:
        for x in d:
            if x:
                events += 1
    print(f"scenarios: {DAYS} days, {crisis} crisis days, {events} issuer defaults")

    rates = Desk("rates", 11, ["RT-NY", "RT-LDN", "RT-TKY"], ["swap", "bond", "future"], 0, 4, False, 600000)
    equity = Desk("equity", 23, ["EQ-D1", "EQ-VOL", "EQ-ARB"], ["stock", "call", "put"], 4, 7, False, 800000)
    fx = Desk("fx", 37, ["FX-SPOT", "FX-OPT"], ["spot", "fwd", "option"], 7, 10, False, 900000)
    credit = Desk("credit", 41, ["CR-IG", "CR-HY", "CR-IDX"], ["cds", "bond"], 10, 12, True, 600000)

    p1 = build_positions(rates)
    p2 = build_positions(equity)
    p3 = build_positions(fx)
    p4 = build_positions(credit)

    write_text("rates.csv", encode_positions(p1))
    write_text("equity.csv", encode_positions(p2))
    write_text("fx.csv", encode_positions(p3))
    write_text("credit.csv", encode_positions(p4))

    whole = read_text("equity.csv")
    write_text("equity.bad", damage(whole, 150, 5, "1.5e3"))

    t1 = read_text("rates.csv")
    t2 = read_text("equity.csv")
    t3 = read_text("fx.csv")
    t4 = read_text("credit.csv")
    print(f"rates.csv: {len(t1.encode())} bytes, fnv {text_sum(t1):08x}")
    print(f"equity.csv: {len(t2.encode())} bytes, fnv {text_sum(t2):08x}")
    print(f"fx.csv: {len(t3.encode())} bytes, fnv {text_sum(t3):08x}")
    print(f"credit.csv: {len(t4.encode())} bytes, fnv {text_sum(t4):08x}")

    g1 = parse_positions("rates.csv", rates, t1)
    g2 = parse_positions("equity.csv", equity, t2)
    g3 = parse_positions("fx.csv", fx, t3)
    g4 = parse_positions("credit.csv", credit, t4)
    try:
        parse_positions("equity.bad", equity, read_text("equity.bad"))
        print("equity.bad: parsed")
    except RiskError as e:
        print(e.what)
    try:
        parse_positions("fx.csv", credit, t3)
        print("fx.csv as credit: parsed")
    except RiskError as e:
        print(e.what)
    same = same_positions(g1, p1) and same_positions(g2, p2) and same_positions(g3, p3) and same_positions(g4, p4)
    print(f"parsed positions match the built ones: {'true' if same else 'false'}")

    e1 = exposures(rates, g1)
    e2 = exposures(equity, g2)
    e3 = exposures(fx, g3)
    e4 = exposures(credit, g4)

    v1 = price(rates, g1, sc)
    v2 = price(equity, g2, sc)
    v3 = price(fx, g3, sc)
    v4 = price(credit, g4, sc)

    r1 = risk(v1)
    r2 = risk(v2)
    r3 = risk(v3)
    r4 = risk(v4)

    lines = [
        totals_report(rates, g1), totals_report(equity, g2), totals_report(fx, g3), totals_report(credit, g4),
        factor_report(rates, e1), factor_report(equity, e2), factor_report(fx, e3), factor_report(credit, e4),
        book_report(rates, r1), book_report(equity, r2), book_report(fx, r3), book_report(credit, r4),
        worst_report(rates, v1, sc), worst_report(equity, v2, sc), worst_report(fx, v3, sc),
        worst_report(credit, v4, sc),
        stress_report(rates, g1), stress_report(equity, g2), stress_report(fx, g3), stress_report(credit, g4),
    ]
    for line in lines:
        print(line)

    lb = LimitBook(FIRM_CAP)
    print(book(lb, rates, v1, r1))
    print(book(lb, equity, v2, r2))
    print(book(lb, fx, v3, r3))
    print(book(lb, credit, v4, r4))
    write_text("limits.log", lb.log)
    back = read_text("limits.log")
    rows = len(back.split("\n")) - 1
    print(f"firm VaR {money(lb.firm_var)} against {money(lb.standalone)} standalone, "
          f"diversification {money(lb.standalone - lb.firm_var)}; {lb.tickets} tickets")
    print(f"limits.log: {rows} entries, {len(back.encode())} bytes, fnv {text_sum(back):08x}")

    for p in ["rates.csv", "equity.csv", "fx.csv", "credit.csv", "equity.bad", "limits.log"]:
        try:
            os.remove(p)
        except OSError:
            pass


def main():
    try:
        run()
    except RiskError as e:
        print(f"risk_batch failed: {e.what}")


main()
