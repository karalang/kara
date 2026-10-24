"""pipeline/replenish_plan, the Python mirror: a weekly replenishment batch
over four distribution centres. It wrote expected.out.

Run from this directory: python3 -I -B replenish_plan.py > expected.out"""
import os

MASK32 = 4294967295
SKUS = 120
WEEKS = 52
HOLDOUT = 26
HORIZON = 4
CATS = ["grocery", "household", "pets", "garden", "outdoor", "toys", "electronics"]
PRICE = [450, 899, 1599, 2499, 4999, 2999, 8999]          # cents
BASE = [60, 30, 20, 14, 8, 10, 6]                          # weekly units, upper bound
AMP = [10, 15, 5, 55, 60, 40, 25]                          # seasonal swing, percent
PEAK = [50, 10, 30, 20, 26, 48, 47]                        # week of the seasonal peak
LEAD = [1, 2, 2, 3, 4, 3, 4]                               # supplier lead time, weeks
CASE = [12, 6, 4, 2, 1, 4, 1]                              # case pack
FAMILY = [0, 0, 0, 1, 1, 2, 2]
FAMILIES = ["dry", "seasonal", "hard"]
CAPACITY = [9000, 1100, 3000]                                # supplier units this week
Z = [2054, 1645, 1282]                                     # service factor x1000: A 98%, B 95%, C 90%
ALPHAS = [10, 20, 30, 40, 50, 60, 70, 80, 90]
BETAS = [0, 10, 20, 30, 40]
HEADER = "sku,cat,on_hand,on_order," + ",".join("w%d" % (w + 1) for w in range(WEEKS))


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


def dollars(cents):
    sign = "-" if cents < 0 else ""
    return sign + "$" + group3(abs(cents) // 100)


def isqrt(n):
    # Newton's method from above; the same steps as the Kara port.
    if n < 2:
        return n
    x = n
    y = (x + 1) // 2
    while y < x:
        x = y
        y = (x + n // x) // 2
    return x


class Dc:
    def __init__(self, name, code, seed, cats):
        self.name = name
        self.code = code
        self.seed = seed
        self.cats = cats


class Sku:
    def __init__(self, sku, cat, on_hand, on_order, sales):
        self.sku = sku
        self.cat = cat
        self.on_hand = on_hand
        self.on_order = on_order
        self.sales = sales        # units per week, -1 for a stockout week


class Fit:
    def __init__(self, alpha, beta, level, trend, abs_err, actual, n):
        self.alpha = alpha
        self.beta = beta
        self.level = level        # milli-units
        self.trend = trend
        self.abs_err = abs_err    # milli-units over the holdout weeks
        self.actual = actual      # units over the holdout weeks
        self.n = n                # holdout weeks with sales on record


class Line:
    def __init__(self, sku, cat, cls, rop, position, qty, value):
        self.sku = sku
        self.cat = cat
        self.cls = cls
        self.rop = rop
        self.position = position
        self.qty = qty
        self.value = value


class Supplier:
    def __init__(self):
        self.left = list(CAPACITY)
        self.pos = 0
        self.backorders = 0
        self.log = ""


class BadData(Exception):
    pass


# ------------------------------------------------------------ the data

def season(cat, w):
    d = (w - PEAK[cat]) % WEEKS
    near = 26 - min(d, WEEKS - d)
    return 100 + tdiv(AMP[cat] * (2 * near - 26), 26)


def build_skus(dc):
    out = []
    for i in range(SKUS):
        h = mix(dc.seed, i, 1)
        cat = CATS.index(dc.cats[h % len(dc.cats)])
        base = 2 + (h >> 4) % BASE[cat]
        sporadic = (h >> 12) % 6 == 0
        sales = []
        for w in range(WEEKS):
            hw = mix(dc.seed, i * WEEKS + w, 2)
            if hw % 31 == 0:
                sales.append(-1)
                continue
            noise = (hw >> 5) % 41 - 20
            units = tdiv(base * season(cat, w) * (100 + noise), 10000)
            if (hw >> 11) % 19 == 0:
                units *= 2
            if sporadic:
                units = units * 3 if (hw >> 16) % 4 == 0 else 0
            sales.append(units)
        h2 = mix(dc.seed, i, 3)
        on_hand = (h2 % 9) * base
        on_order = CASE[cat] * ((h2 >> 8) % 3) if (h2 >> 4) % 4 == 0 else 0
        out.append(Sku("%s-%04d" % (dc.code, i + 1), CATS[cat], on_hand, on_order, sales))
    return out


def encode_skus(skus):
    out = HEADER + "\n"
    for s in skus:
        weeks = ",".join("-" if x < 0 else str(x) for x in s.sales)
        out += "%s,%s,%d,%d,%s\n" % (s.sku, s.cat, s.on_hand, s.on_order, weeks)
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
    f = lines[n + 1].split(",")
    f[field] = value
    lines[n + 1] = ",".join(f)
    return "\n".join(lines)


def count_field(path, n, what, text):
    if text == "" or not all("0" <= c <= "9" for c in text):
        raise BadData("%s line %d: bad %s '%s'" % (path, n, what, text))
    return int(text)


def parse_skus(path, dc, text):
    lines = text.split("\n")
    if lines[0] != HEADER:
        raise BadData(path + ": bad header")
    out = []
    for k in range(1, len(lines)):
        line = lines[k]
        if line == "":
            continue
        n = k + 1
        f = line.split(",")
        if len(f) != 4 + WEEKS:
            raise BadData("%s line %d: %d fields" % (path, n, len(f)))
        sku = f[0]
        if len(sku) != 7 or sku[:3] != dc.code + "-" or not all("0" <= c <= "9" for c in sku[3:]):
            raise BadData("%s line %d: bad sku '%s'" % (path, n, sku))
        if f[1] not in dc.cats:
            raise BadData("%s line %d: %s is not stocked at %s" % (path, n, f[1], dc.name))
        on_hand = count_field(path, n, "on_hand", f[2])
        on_order = count_field(path, n, "on_order", f[3])
        sales = []
        for w in range(WEEKS):
            x = f[4 + w]
            if x == "-":
                sales.append(-1)
            else:
                sales.append(count_field(path, n, "w%d" % (w + 1), x))
        out.append(Sku(sku, f[1], on_hand, on_order, sales))
    return out


def same_skus(a, b):
    if len(a) != len(b):
        return False
    for x, y in zip(a, b):
        if (x.sku, x.cat, x.on_hand, x.on_order, x.sales) != (y.sku, y.cat, y.on_hand, y.on_order, y.sales):
            return False
    return True


# ------------------------------------------------------------ forecasting

# Holt's linear smoothing in milli-units, one run. Stockout weeks carry the
# forecast forward without an update.
def holt(sales, alpha, beta):
    level = -1
    trend = 0
    abs_err = 0
    actual = 0
    n = 0
    for w in range(WEEKS):
        y = sales[w]
        if level < 0:
            if y >= 0:
                level = y * 1000
            continue
        f = level + trend
        if y < 0:
            level = f
            continue
        err = y * 1000 - f
        if w >= HOLDOUT:
            abs_err += abs(err)
            actual += y
            n += 1
        new_level = f + tdiv(alpha * err, 100)
        trend = trend + tdiv(beta * (new_level - level - trend), 100)
        level = new_level
    if level < 0:
        level = 0
    return Fit(alpha, beta, level, trend, abs_err, actual, n)


# Every alpha and beta on the grid; the lowest holdout error wins, the first
# in grid order on a tie.
def fit(sales):
    best = None
    for a in ALPHAS:
        for b in BETAS:
            f = holt(sales, a, b)
            if best is None or f.abs_err < best.abs_err:
                best = f
    return best


def forecast_all(skus):
    return [fit(s.sales) for s in skus]


def ahead(f, h):
    x = f.level + h * f.trend
    return x if x > 0 else 0


# ------------------------------------------------------------ planning

def revenue(s):
    units = sum(x for x in s.sales if x > 0)
    return units * PRICE[CATS.index(s.cat)]


# A by revenue to 70%, B to 90%, C the rest.
def classify(skus):
    order = sorted(range(len(skus)), key=lambda i: (-revenue(skus[i]), skus[i].sku))
    total = sum(revenue(s) for s in skus)
    cls = [2] * len(skus)
    run = 0
    for i in order:
        if run * 100 < total * 70:
            cls[i] = 0
        elif run * 100 < total * 90:
            cls[i] = 1
        run += revenue(skus[i])
    return cls


def plan(skus, fits, cls):
    out = []
    for i in range(len(skus)):
        s = skus[i]
        f = fits[i]
        c = CATS.index(s.cat)
        lead = LEAD[c]
        demand = 0
        for h in range(1, lead + 2):
            demand += ahead(f, h)
        mad = f.abs_err // f.n if f.n > 0 else 0
        sigma = mad * 1250 // 1000
        safety = Z[cls[i]] * sigma // 1000 * isqrt((lead + 1) * 10000) // 100
        rop = (demand + safety + 999) // 1000
        position = s.on_hand + s.on_order
        qty = 0
        if position <= rop:
            target = rop
            for h in range(lead + 2, lead + 2 + HORIZON):
                target += (ahead(f, h) + 999) // 1000
            need = target - position
            qty = (need + CASE[c] - 1) // CASE[c] * CASE[c]
        out.append(Line(s.sku, s.cat, cls[i], rop, position, qty, qty * PRICE[c]))
    return out


# ------------------------------------------------------------ reports

def sales_report(dc, skus):
    units = 0
    rev = 0
    stockouts = 0
    by_cat = {}
    for s in skus:
        for x in s.sales:
            if x < 0:
                stockouts += 1
            else:
                units += x
        r = revenue(s)
        rev += r
        by_cat[s.cat] = by_cat.get(s.cat, 0) + r
    top = sorted(by_cat.items(), key=lambda kv: (-kv[1], kv[0]))[0]
    return "%s sales: %s units, %s, %d stockout weeks; top %s %s" % (
        dc.name, group3(units), dollars(rev), stockouts, top[0], dollars(top[1]))


def abc_report(dc, skus, cls):
    total = sum(revenue(s) for s in skus)
    parts = []
    for k in range(3):
        n = sum(1 for c in cls if c == k)
        r = sum(revenue(skus[i]) for i in range(len(skus)) if cls[i] == k)
        parts.append("%s %d skus %d.%d%%" % ("ABC"[k], n, r * 100 // total, r * 1000 // total % 10))
    return dc.name + " abc: " + ", ".join(parts)


def accuracy_report(dc, skus, fits):
    parts = []
    for cat in dc.cats:
        err = 0
        act = 0
        for i in range(len(skus)):
            if skus[i].cat == cat:
                err += fits[i].abs_err
                act += fits[i].actual
        wmape = err // act if act > 0 else 0
        parts.append("%s %d.%d%%" % (cat, wmape // 10, wmape % 10))
    counts = {}
    for f in fits:
        counts[(f.alpha, f.beta)] = counts.get((f.alpha, f.beta), 0) + 1
    (a, b), n = sorted(counts.items(), key=lambda kv: (-kv[1], kv[0]))[0]
    return "%s wmape: %s; most chosen alpha .%02d beta .%02d (%d skus)" % (
        dc.name, ", ".join(parts), a, b, n)


def order_report(dc, lines):
    picks = [l for l in lines if l.qty > 0]
    units = sum(l.qty for l in picks)
    value = sum(l.value for l in picks)
    top = sorted(picks, key=lambda l: (-l.value, l.sku))[:3]
    tops = ", ".join("%s %d %s" % (l.sku, l.qty, dollars(l.value)) for l in top)
    return "%s orders: %d lines, %s units, %s; largest %s" % (
        dc.name, len(picks), group3(units), dollars(value), tops)


def cover_report(dc, skus, fits):
    dead = 0
    dead_value = 0
    buckets = [0, 0, 0, 0]
    for i in range(len(skus)):
        s = skus[i]
        price = PRICE[CATS.index(s.cat)]
        recent = sum(x for x in s.sales[WEEKS - 8:] if x > 0)
        if recent == 0 and s.on_hand > 0:
            dead += 1
            dead_value += s.on_hand * price
        weekly = ahead(fits[i], 1)
        if weekly == 0:
            buckets[3] += 1
            continue
        weeks = s.on_hand * 1000 // weekly
        if weeks < 1:
            buckets[0] += 1
        elif weeks < 4:
            buckets[1] += 1
        else:
            buckets[2] += 1
    return "%s cover: %d under a week, %d under four, %d longer, %d no demand; %d dead skus holding %s" % (
        dc.name, buckets[0], buckets[1], buckets[2], buckets[3], dead, dollars(dead_value))


# ------------------------------------------------------------ the supplier

# One supplier fills purchase orders in arrival order from this week's
# capacity per family. A short line raises a numbered backorder.
def submit(sup, dc, lines):
    sup.pos += 1
    po = "PO-%04d" % sup.pos
    filled = 0
    asked = 0
    shorts = []
    for l in sorted(lines, key=lambda l: l.sku):
        if l.qty == 0:
            continue
        c = CATS.index(l.cat)
        fam = FAMILY[c]
        asked += l.qty
        got = min(l.qty, sup.left[fam]) // CASE[c] * CASE[c]
        sup.left[fam] -= got
        filled += got
        sup.log += "%s %s %s %d/%d\n" % (po, dc.name, l.sku, got, l.qty)
        if got < l.qty:
            sup.backorders += 1
            bo = "BO-%04d" % sup.backorders
            sup.log += "%s %s %s short %d\n" % (bo, dc.name, l.sku, l.qty - got)
            shorts.append(bo)
    left = ", ".join("%s %d" % (FAMILIES[k], sup.left[k]) for k in range(3))
    first = (" from " + shorts[0]) if shorts else ""
    plural = "" if len(shorts) == 1 else "s"
    return "%s %s: %s of %s units filled, %d backorder%s%s; left %s" % (
        po, dc.name, group3(filled), group3(asked), len(shorts), plural, first, left)


# ------------------------------------------------------------ the job

def run():
    north = Dc("north", "NO", 13, ["grocery", "household", "pets"])
    south = Dc("south", "SO", 29, ["grocery", "garden", "outdoor"])
    east = Dc("east", "EA", 43, ["household", "toys", "electronics"])
    west = Dc("west", "WE", 57, ["electronics", "garden", "pets"])

    k1 = build_skus(north)
    k2 = build_skus(south)
    k3 = build_skus(east)
    k4 = build_skus(west)

    write_text("north.csv", encode_skus(k1))
    write_text("south.csv", encode_skus(k2))
    write_text("east.csv", encode_skus(k3))
    write_text("west.csv", encode_skus(k4))

    whole = read_text("east.csv")
    write_text("east.bad", damage(whole, 77, 30, "12.5"))

    t1 = read_text("north.csv")
    t2 = read_text("south.csv")
    t3 = read_text("east.csv")
    t4 = read_text("west.csv")
    for name, t in [("north", t1), ("south", t2), ("east", t3), ("west", t4)]:
        print("%s.csv: %d bytes, fnv %08x" % (name, len(t.encode()), text_sum(t)))

    g1 = parse_skus("north.csv", north, t1)
    g2 = parse_skus("south.csv", south, t2)
    g3 = parse_skus("east.csv", east, t3)
    g4 = parse_skus("west.csv", west, t4)
    try:
        parse_skus("east.bad", east, read_text("east.bad"))
        print("east.bad: parsed")
    except BadData as e:
        print(e)
    try:
        parse_skus("south.csv", west, t2)
        print("south.csv as west: parsed")
    except BadData as e:
        print(e)
    same = same_skus(g1, k1) and same_skus(g2, k2) and same_skus(g3, k3) and same_skus(g4, k4)
    print("parsed skus match the built ones: %s" % ("true" if same else "false"))

    f1 = forecast_all(g1)
    f2 = forecast_all(g2)
    f3 = forecast_all(g3)
    f4 = forecast_all(g4)

    c1 = classify(g1)
    c2 = classify(g2)
    c3 = classify(g3)
    c4 = classify(g4)

    l1 = plan(g1, f1, c1)
    l2 = plan(g2, f2, c2)
    l3 = plan(g3, f3, c3)
    l4 = plan(g4, f4, c4)

    reports = [
        sales_report(north, g1), sales_report(south, g2), sales_report(east, g3), sales_report(west, g4),
        abc_report(north, g1, c1), abc_report(south, g2, c2), abc_report(east, g3, c3), abc_report(west, g4, c4),
        accuracy_report(north, g1, f1), accuracy_report(south, g2, f2), accuracy_report(east, g3, f3),
        accuracy_report(west, g4, f4),
        order_report(north, l1), order_report(south, l2), order_report(east, l3), order_report(west, l4),
        cover_report(north, g1, f1), cover_report(south, g2, f2), cover_report(east, g3, f3),
        cover_report(west, g4, f4),
    ]
    for r in reports:
        print(r)

    sup = Supplier()
    print(submit(sup, north, l1))
    print(submit(sup, south, l2))
    print(submit(sup, east, l3))
    print(submit(sup, west, l4))
    write_text("supplier.log", sup.log)
    back = read_text("supplier.log")
    rows = len(back.split("\n")) - 1
    print("supplier.log: %d entries, %d bytes, fnv %08x" % (rows, len(back.encode()), text_sum(back)))

    for p in ["north.csv", "south.csv", "east.csv", "west.csv", "east.bad", "supplier.log"]:
        os.remove(p)


try:
    run()
except BadData as e:
    print("replenish_plan failed: %s" % e)
