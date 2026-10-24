"""pipeline/claim_batch, the Python mirror: an insurer's yearly claims
batch over four branches, from policy and claim files to fraud scoring,
adjudication, reports and one shared stop-loss reinsurance treaty. It
wrote expected.out.

Run from this directory: python3 -I -B claim_batch.py > expected.out"""
import os

MASK32 = 4294967295
POLICIES = 2400
CLAIMS = 1300
VENDORS = 40
LINES = ["auto", "home", "travel"]
PERILS = ["collision", "theft", "glass", "water", "fire", "storm", "medical", "baggage"]
# The perils each line covers.
COVERS = [
    [0, 1, 2, 4, 5],
    [1, 2, 3, 4, 5],
    [1, 6, 7],
]
# Per line: sum insured range in dollars, deductible choices in dollars,
# the insurer's share in percent, and the premium per $1000 insured.
SUM_LO = [8000, 150000, 5000]
SUM_SPAN = [32000, 750000, 45000]
DEDUCTIBLES = [[250, 500, 1000], [500, 1000, 2500], [0, 100, 250]]
COINSURANCE = [[100], [80, 90, 100], [100]]
RATE_PER_1000 = [160, 24, 90]
# Per peril: claim amount range in dollars.
AMOUNT_LO = [600, 300, 150, 1500, 4000, 2000, 400, 200]
AMOUNT_SPAN = [9000, 5000, 1200, 30000, 120000, 40000, 25000, 2500]
# Fraud points.
LATE_REPORT_DAYS = 30
NEW_POLICY_DAYS = 30
SIMILAR_DAYS = 10
REPEAT_DAYS = 45
REFER_AT = 50
REVIEW_AT = 35
NOTICE_LIMIT = 90
# The stop-loss treaty over the year's net paid claims of all four
# branches: the insurer keeps the first RETENTION, the reinsurer pays
# SHARE percent of what lies above it, up to LIMIT in all.
RETENTION = 3000000000
SHARE = 80
LIMIT = 900000000
MONTHS = [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]


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


def pct(a, b):
    if b == 0:
        return "0.0%"
    q = a * 1000 // b
    return f"{q // 10}.{q % 10}%"


def is_leap(y):
    return y % 4 == 0 and (y % 100 != 0 or y % 400 == 0)


def year_len(y):
    return 366 if is_leap(y) else 365


def month_len(y, m):
    if m == 2 and is_leap(y):
        return 29
    return MONTHS[m - 1]


# Day 0 is 2026-01-01; negative days fall in 2025.
def date_of(d):
    y = 2026
    while d < 0:
        y -= 1
        d += year_len(y)
    while d >= year_len(y):
        d -= year_len(y)
        y += 1
    m = 1
    while d >= month_len(y, m):
        d -= month_len(y, m)
        m += 1
    return f"{y}-{m:02}-{d + 1:02}"


def is_digits(t):
    return len(t) > 0 and all("0" <= c <= "9" for c in t)


def day_of(t):
    """The day number of a YYYY-MM-DD date, or None."""
    if len(t) != 10 or t[4] != "-" or t[7] != "-":
        return None
    ys, ms, ds = t[0:4], t[5:7], t[8:10]
    if not (is_digits(ys) and is_digits(ms) and is_digits(ds)):
        return None
    y, m, dd = int(ys), int(ms), int(ds)
    if y < 2000 or y > 2100 or m < 1 or m > 12 or dd < 1 or dd > month_len(y, m):
        return None
    d = dd - 1
    for k in range(1, m):
        d += month_len(y, k)
    yy = 2026
    while yy > y:
        yy -= 1
        d -= year_len(yy)
    while yy < y:
        d += year_len(yy)
        yy += 1
    return d


def dollars(c):
    return f"{c // 100}.{c % 100:02}"


def cents_of(t):
    """Dollars with exactly two decimals, as cents, or None."""
    dot = t.find(".")
    if dot < 1 or len(t) - dot != 3:
        return None
    a, b = t[:dot], t[dot + 1 :]
    if not (is_digits(a) and is_digits(b)):
        return None
    return int(a) * 100 + int(b)


class Branch:
    def __init__(self, name, code, seed, mix_pct, storm_day, ring):
        self.name = name
        self.code = code
        self.seed = seed
        self.mix_pct = mix_pct
        self.storm_day = storm_day
        self.ring = ring


def pick_line(br, r):
    x = r % 100
    if x < br.mix_pct[0]:
        return 0
    if x < br.mix_pct[0] + br.mix_pct[1]:
        return 1
    return 2


def build_policies(br):
    out = []
    for i in range(POLICIES):
        line = pick_line(br, mix(br.seed, i, 1))
        sum_dollars = SUM_LO[line] + mix(br.seed, i, 2) % SUM_SPAN[line]
        sum_dollars -= sum_dollars % 100
        ded = DEDUCTIBLES[line][mix(br.seed, i, 3) % len(DEDUCTIBLES[line])]
        coins = COINSURANCE[line][mix(br.seed, i, 4) % len(COINSURANCE[line])]
        start = -300 + mix(br.seed, i, 5) % 420
        premium = sum_dollars * 100 * RATE_PER_1000[line] // 1000
        out.append(
            {
                "pid": f"{br.code}-P{i + 1:04}",
                "line": line,
                "sum": sum_dollars * 100,
                "ded": ded * 100,
                "coins": coins,
                "start": start,
                "premium": premium,
            }
        )
    return out


def build_claims(br, policies):
    out = []

    def pi_of(pid):
        k = int(pid[4:]) - 1
        return k if k < len(policies) else None

    for i in range(CLAIMS):
        r = mix(br.seed, i, 11)
        if r % 200 == 0:
            pid = f"{br.code}-P{POLICIES + 1 + r % 50:04}"
            line = r % 3
        else:
            p = policies[r % POLICIES]
            pid = p["pid"]
            line = p["line"]
        cov = COVERS[line]
        r2 = mix(br.seed, i, 12)
        if r2 % 25 == 0:
            peril = (cov[0] + 3) % len(PERILS)
            if peril in cov:
                peril = 3 if line != 1 else 0
        else:
            peril = cov[r2 % len(cov)]
        # Most losses fall in the policy's period; some do not.
        r4 = mix(br.seed, i, 13)
        if r4 % 25 == 0 or pi_of(pid) is None:
            loss = r4 % 365
        else:
            st = policies[pi_of(pid)]["start"]
            lo = max(0, st)
            hi = min(364, st + 364)
            loss = lo + r4 % (hi - lo + 1)
        if line != 2 and mix(br.seed, i, 14) % 7 == 0:
            peril = 5
            loss = br.storm_day + mix(br.seed, i, 15) % 3
        r3 = mix(br.seed, i, 16)
        lag = r3 % 12 if r3 % 10 < 8 else 20 + r3 % 100
        amount = (AMOUNT_LO[peril] + mix(br.seed, i, 17) % AMOUNT_SPAN[peril]) * 100 + mix(br.seed, i, 18) % 100
        if mix(br.seed, i, 19) % 17 == 0:
            amount = (amount // 100000 + 1) * 100000
        vendor = 1 + mix(br.seed, i, 20) % VENDORS
        items = 1 + mix(br.seed, i, 21) % 5
        out.append(
            {
                "cid": f"{br.code}-C{i + 1:05}",
                "pid": pid,
                "peril": peril,
                "loss": loss,
                "report": loss + lag,
                "amount": amount,
                "vendor": vendor,
                "items": items,
            }
        )
    # A ring: one vendor files near-identical collision or water claims on
    # different policies a few days apart.
    k = 0
    for j in range(36):
        p = policies[mix(br.seed, j, 31) % POLICIES]
        peril = 0 if p["line"] == 0 else (3 if p["line"] == 1 else 7)
        day = br.storm_day + 40 + (j // 6) * 9 + j % 6
        base = 742000 if peril == 0 else (1868000 if peril == 3 else 185000)
        amount = base + (mix(br.seed, j, 32) % 1200) * 10
        out.append(
            {
                "cid": f"{br.code}-C{CLAIMS + 1 + k:05}",
                "pid": p["pid"],
                "peril": peril,
                "loss": day,
                "report": day + 1 + j % 3,
                "amount": amount,
                "vendor": br.ring,
                "items": 2,
            }
        )
        k += 1
    return out


def policy_header():
    return "policy,line,sum_insured,deductible,coinsurance,start,premium"


def claim_header():
    return "claim,policy,peril,loss_date,report_date,amount,vendor,items"


def encode_policies(ps):
    out = [policy_header()]
    for p in ps:
        out.append(
            f"{p['pid']},{LINES[p['line']]},{dollars(p['sum'])},{dollars(p['ded'])},{p['coins']},{date_of(p['start'])},{dollars(p['premium'])}"
        )
    return "\n".join(out) + "\n"


def encode_claims(cs):
    out = [claim_header()]
    for c in cs:
        out.append(
            f"{c['cid']},{c['pid']},{PERILS[c['peril']]},{date_of(c['loss'])},{date_of(c['report'])},{dollars(c['amount'])},V-{c['vendor']:03},{c['items']}"
        )
    return "\n".join(out) + "\n"


class BatchError(Exception):
    pass


def read_text(path):
    try:
        with open(path, encoding="utf-8", newline="") as f:
            return f.read()
    except OSError:
        raise BatchError(f"could not read {path}")


def write_text(path, text):
    with open(path, "w", encoding="utf-8", newline="") as f:
        f.write(text)


def damage(text, n, field, value):
    lines = text.split("\n")
    f = lines[n].split(",")
    f[field] = value
    lines[n] = ",".join(f)
    return "\n".join(lines)


def id_ok(t, code, letter, digits):
    return len(t) == 4 + digits and t.startswith(f"{code}-{letter}") and is_digits(t[4:])


def parse_policies(path, br, text):
    lines = text.split("\n")
    if lines[0] != policy_header():
        raise BatchError(f"{path}: bad header")
    out = []
    for k in range(1, len(lines)):
        line = lines[k]
        if line == "":
            continue
        n = k + 1
        f = line.split(",")
        if len(f) != 7:
            raise BatchError(f"{path} line {n}: {len(f)} fields")
        if not id_ok(f[0], br.code, "P", 4):
            raise BatchError(f"{path} line {n}: bad policy '{f[0]}'")
        if f[1] not in LINES:
            raise BatchError(f"{path} line {n}: bad line '{f[1]}'")
        vals = []
        for j, what in ((2, "sum insured"), (3, "deductible"), (6, "premium")):
            v = cents_of(f[j])
            if v is None:
                raise BatchError(f"{path} line {n}: bad {what} '{f[j]}'")
            vals.append(v)
        if not is_digits(f[4]) or int(f[4]) < 1 or int(f[4]) > 100:
            raise BatchError(f"{path} line {n}: bad coinsurance '{f[4]}'")
        start = day_of(f[5])
        if start is None:
            raise BatchError(f"{path} line {n}: bad start '{f[5]}'")
        out.append(
            {
                "pid": f[0],
                "line": LINES.index(f[1]),
                "sum": vals[0],
                "ded": vals[1],
                "coins": int(f[4]),
                "start": start,
                "premium": vals[2],
            }
        )
    return out


def parse_claims(path, br, text):
    lines = text.split("\n")
    if lines[0] != claim_header():
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
        if not id_ok(f[0], br.code, "C", 5):
            raise BatchError(f"{path} line {n}: bad claim '{f[0]}'")
        if not id_ok(f[1], br.code, "P", 4):
            raise BatchError(f"{path} line {n}: bad policy '{f[1]}'")
        if f[2] not in PERILS:
            raise BatchError(f"{path} line {n}: bad peril '{f[2]}'")
        loss = day_of(f[3])
        if loss is None:
            raise BatchError(f"{path} line {n}: bad loss date '{f[3]}'")
        report = day_of(f[4])
        if report is None:
            raise BatchError(f"{path} line {n}: bad report date '{f[4]}'")
        if report < loss:
            raise BatchError(f"{path} line {n}: reported before the loss")
        amount = cents_of(f[5])
        if amount is None:
            raise BatchError(f"{path} line {n}: bad amount '{f[5]}'")
        if len(f[6]) != 5 or not f[6].startswith("V-") or not is_digits(f[6][2:]):
            raise BatchError(f"{path} line {n}: bad vendor '{f[6]}'")
        if not is_digits(f[7]):
            raise BatchError(f"{path} line {n}: bad items '{f[7]}'")
        out.append(
            {
                "cid": f[0],
                "pid": f[1],
                "peril": PERILS.index(f[2]),
                "loss": loss,
                "report": report,
                "amount": amount,
                "vendor": int(f[6][2:]),
                "items": int(f[7]),
            }
        )
    return out


def policy_index(ps):
    return {p["pid"]: i for i, p in enumerate(ps)}


# ------------------------------------------------------------ fraud scoring


def score_claims(ps, cs):
    """Each claim's fraud points, in file order, with the reasons."""
    idx = policy_index(ps)
    order = sorted(range(len(cs)), key=lambda i: (cs[i]["loss"], i))
    scores = [0] * len(cs)
    similar = [0] * len(cs)
    reasons = [[] for _ in cs]
    # similar claims: same vendor and peril, a different policy, within
    # SIMILAR_DAYS of the loss and within 2% of the amount
    for a in range(len(order)):
        i = order[a]
        ci = cs[i]
        hits = 0
        for dirn in (-1, 1):
            b = a + dirn
            while 0 <= b < len(order):
                cj = cs[order[b]]
                if abs(cj["loss"] - ci["loss"]) > SIMILAR_DAYS:
                    break
                if (
                    cj["vendor"] == ci["vendor"]
                    and cj["peril"] == ci["peril"]
                    and cj["pid"] != ci["pid"]
                    and abs(cj["amount"] - ci["amount"]) * 50 <= ci["amount"]
                ):
                    hits += 1
                b += dirn
        similar[i] = hits
    # repeat claims: the same policy and peril within REPEAT_DAYS before
    last = {}
    for i in order:
        c = cs[i]
        key = (c["pid"], c["peril"])
        if key in last and c["loss"] - last[key] <= REPEAT_DAYS:
            scores[i] += 20
            reasons[i].append("repeat")
        last[key] = c["loss"]
    for i, c in enumerate(cs):
        if c["report"] - c["loss"] > LATE_REPORT_DAYS:
            scores[i] += 15
            reasons[i].append("late")
        pi = idx.get(c["pid"])
        if pi is not None:
            p = ps[pi]
            if 0 <= c["loss"] - p["start"] < NEW_POLICY_DAYS:
                scores[i] += 20
                reasons[i].append("new")
            if c["amount"] * 100 >= p["sum"] * 60:
                scores[i] += 15
                reasons[i].append("large")
        if c["amount"] % 100000 == 0:
            scores[i] += 10
            reasons[i].append("round")
        if similar[i] > 0:
            scores[i] += min(40, 10 * similar[i])
            reasons[i].append(f"similar x{similar[i]}")
    return {"scores": scores, "similar": similar, "reasons": reasons}


# ------------------------------------------------------------ adjudication


def adjudicate(ps, cs, sc):
    """Claims in report order (then file order); each policy's limit is
    used up by the claims paid before it."""
    idx = policy_index(ps)
    left = [p["sum"] for p in ps]
    order = sorted(range(len(cs)), key=lambda i: (cs[i]["report"], i))
    out = [None] * len(cs)
    for i in order:
        c = cs[i]
        s = sc["scores"][i]
        pi = idx.get(c["pid"])
        r = {"status": "", "pay": 0, "paid_day": 0, "flag": s >= REVIEW_AT and s < REFER_AT, "capped": False}
        out[i] = r
        if pi is None:
            r["status"] = "no policy"
            continue
        p = ps[pi]
        if c["peril"] not in COVERS[p["line"]]:
            r["status"] = "not covered"
            continue
        if c["loss"] < p["start"] or c["loss"] > p["start"] + 364:
            r["status"] = "outside period"
            continue
        if c["report"] - c["loss"] > NOTICE_LIMIT:
            r["status"] = "late notice"
            continue
        if s >= REFER_AT:
            r["status"] = "referred"
            continue
        covered = c["amount"] - p["ded"]
        if covered <= 0:
            r["status"] = "below deductible"
            continue
        covered = covered * p["coins"] // 100
        pay = min(covered, left[pi])
        if pay <= 0:
            r["status"] = "limit used"
            continue
        r["capped"] = pay < covered
        left[pi] -= pay
        r["status"] = "paid"
        r["pay"] = pay
        r["paid_day"] = c["report"] + 5 + mix(len(c["cid"]) + c["amount"], c["vendor"], 41) % 10 + (14 if r["flag"] else 0)
    return out


# ------------------------------------------------------------ reports


def volume_report(br, cs):
    counts = [0] * len(PERILS)
    total = 0
    for c in cs:
        counts[c["peril"]] += 1
        total += c["amount"]
    top = sorted(range(len(PERILS)), key=lambda k: (-counts[k], k))[:3]
    tops = ", ".join(f"{PERILS[k]} {counts[k]}" for k in top)
    storm = sum(1 for c in cs if c["peril"] == 5 and br.storm_day <= c["loss"] <= br.storm_day + 2)
    return f"{br.name} claims: {group3(len(cs))} for {money(total)}; most {tops}; storm of {date_of(br.storm_day)}: {storm} claims"


def fraud_report(br, cs, sc):
    scores = sc["scores"]
    refer = sum(1 for s in scores if s >= REFER_AT)
    review = sum(1 for s in scores if REVIEW_AT <= s < REFER_AT)
    top = sorted(range(len(cs)), key=lambda i: (-scores[i], i))[:3]
    tops = "; ".join(f"{cs[i]['cid']} {scores[i]} ({' '.join(sc['reasons'][i])})" for i in top)
    per_vendor = {}
    for i, c in enumerate(cs):
        if sc["similar"][i] > 0:
            per_vendor[c["vendor"]] = per_vendor.get(c["vendor"], 0) + 1
    if per_vendor:
        v = sorted(per_vendor, key=lambda k: (-per_vendor[k], k))[0]
        ring = f"V-{v:03} on {per_vendor[v]} similar claims"
    else:
        ring = "none"
    return f"{br.name} fraud: {refer} referred, {review} for review; highest {tops}; likeliest ring {ring}"


def outcome_report(br, ps, cs, res):
    paid = [r for r in res if r["status"] == "paid"]
    total = sum(r["pay"] for r in paid)
    capped = sum(1 for r in paid if r["capped"])
    why = {}
    for r in res:
        if r["status"] != "paid":
            why[r["status"]] = why.get(r["status"], 0) + 1
    whys = ", ".join(f"{k} {why[k]}" for k in sorted(why, key=lambda k: (-why[k], k)))
    premium = sum(p["premium"] for p in ps)
    return f"{br.name} outcomes: {len(paid)} paid for {money(total)} ({capped} at the limit), loss ratio {pct(total, premium)}; not paid: {whys}"


def nth(xs, k):
    return xs[min(len(xs) - 1, k)]


def lag_report(br, cs, res):
    lags = sorted(c["report"] - c["loss"] for c in cs)
    pays = sorted(r["paid_day"] - c["report"] for c, r in zip(cs, res) if r["status"] == "paid")
    n = len(lags)
    m = len(pays)
    return (
        f"{br.name} lags: reported in {nth(lags, n // 2)} days at the median, {nth(lags, n * 9 // 10)} at p90, "
        f"{lags[-1]} at most; paid {nth(pays, m // 2)} days after the report at the median, {nth(pays, m * 9 // 10)} at p90"
    )


def line_report(br, ps, cs, res):
    idx = policy_index(ps)
    parts = []
    for line in range(3):
        n = 0
        total = 0
        for c, r in zip(cs, res):
            pi = idx.get(c["pid"])
            if r["status"] == "paid" and ps[pi]["line"] == line:
                n += 1
                total += r["pay"]
        avg = total // n if n else 0
        parts.append(f"{LINES[line]} {n} paid, {money(total)}, average {money(avg)}")
    return f"{br.name} lines: " + "; ".join(parts)


# ------------------------------------------------------------ the treaty


class Treaty:
    def __init__(self):
        self.net = 0
        self.ceded = 0
        self.recoveries = 0
        self.checks = 0
        self.log = []


def month_of(d):
    return date_of(d)[:7]


def cede(tr, br, cs, res):
    """The branch's payments in payment order go through the one treaty:
    each payment's share above the retention is ceded until the limit is
    used, the ceded amounts are billed to the reinsurer once a month as a
    numbered recovery, and every payment takes the next check number."""
    order = sorted(
        (i for i in range(len(cs)) if res[i]["status"] == "paid"),
        key=lambda i: (res[i]["paid_day"], i),
    )
    paid = 0
    ceded = 0
    first_check = None
    rec_ids = []
    month = None
    bill = 0
    bill_n = 0

    def close():
        nonlocal bill, bill_n
        if bill > 0:
            tr.recoveries += 1
            rid = f"RC-{tr.recoveries:04}"
            rec_ids.append(rid)
            tr.log.append(f"{rid},{br.name},{month},{bill_n},{dollars(bill)}")
        bill = 0
        bill_n = 0

    for i in order:
        pay = res[i]["pay"]
        m = month_of(res[i]["paid_day"])
        if m != month:
            close()
            month = m
        tr.checks += 1
        if first_check is None:
            first_check = tr.checks
        tr.log.append(f"CHK-{tr.checks:06},{cs[i]['cid']},{date_of(res[i]['paid_day'])},{dollars(pay)}")
        before = tr.net
        tr.net += pay
        paid += pay
        over = max(0, tr.net - max(before, RETENTION))
        part = min(over * SHARE // 100, LIMIT - tr.ceded)
        if part > 0:
            tr.ceded += part
            ceded += part
            bill += part
            bill_n += 1
    close()
    if len(rec_ids) > 1:
        first = f" {rec_ids[0]} to {rec_ids[-1]}"
    elif rec_ids:
        first = f" {rec_ids[0]}"
    else:
        first = ""
    plural = "y" if len(rec_ids) == 1 else "ies"
    left = LIMIT - tr.ceded
    checks = f"checks {first_check} to {tr.checks}" if first_check is not None else "no checks"
    return (
        f"{br.name} ceded: {len(order)} payments ({checks}), {money(paid)} paid, {money(ceded)} recovered "
        f"in {len(rec_ids)} monthly recover{plural}{first}; net retained {money(paid - ceded)}; year to date {money(tr.net)}, "
        f"treaty left {money(left)}"
    )


def same(a, b):
    return a == b


def run():
    north = Branch("north", "NB", 11, [55, 30, 15], 201, 7)
    harbor = Branch("harbor", "HB", 23, [40, 45, 15], 255, 19)
    plains = Branch("plains", "PB", 37, [60, 35, 5], 143, 31)
    metro = Branch("metro", "MB", 53, [45, 25, 30], 288, 3)
    branches = [north, harbor, plains, metro]

    ps = [build_policies(b) for b in branches]
    cs = [build_claims(b, p) for b, p in zip(branches, ps)]

    for b, p, c in zip(branches, ps, cs):
        write_text(f"{b.name}_policies.csv", encode_policies(p))
        write_text(f"{b.name}_claims.csv", encode_claims(c))
    whole = read_text("harbor_claims.csv")
    write_text("harbor_claims.bad", damage(whole, 412, 5, "1250.5O"))

    tp = [read_text(f"{b.name}_policies.csv") for b in branches]
    tc = [read_text(f"{b.name}_claims.csv") for b in branches]
    for b, a, c in zip(branches, tp, tc):
        print(
            f"{b.name}: policies {group3(len(a))} bytes, fnv {text_sum(a):08x}; claims {group3(len(c))} bytes, fnv {text_sum(c):08x}"
        )

    gp = [parse_policies(f"{b.name}_policies.csv", b, t) for b, t in zip(branches, tp)]
    gc = [parse_claims(f"{b.name}_claims.csv", b, t) for b, t in zip(branches, tc)]
    try:
        parse_claims("harbor_claims.bad", harbor, read_text("harbor_claims.bad"))
        print("harbor_claims.bad: parsed")
    except BatchError as e:
        print(e)
    try:
        parse_policies("metro_policies.csv", north, tp[3])
        print("metro_policies.csv as north: parsed")
    except BatchError as e:
        print(e)
    ok = all(same(a, b) for a, b in zip(gp, ps)) and all(same(a, b) for a, b in zip(gc, cs))
    print(f"parsed files match the built ones: {'true' if ok else 'false'}")

    sc = [score_claims(p, c) for p, c in zip(gp, gc)]
    res = [adjudicate(p, c, s) for p, c, s in zip(gp, gc, sc)]

    lines = []
    for b, c in zip(branches, gc):
        lines.append(volume_report(b, c))
    for b, c, s in zip(branches, gc, sc):
        lines.append(fraud_report(b, c, s))
    for b, p, c, r in zip(branches, gp, gc, res):
        lines.append(outcome_report(b, p, c, r))
    for b, c, r in zip(branches, gc, res):
        lines.append(lag_report(b, c, r))
    for b, p, c, r in zip(branches, gp, gc, res):
        lines.append(line_report(b, p, c, r))
    for line in lines:
        print(line)

    tr = Treaty()
    for b, c, r in zip(branches, gc, res):
        print(cede(tr, b, c, r))
    write_text("treaty.log", "\n".join(tr.log) + "\n")
    back = read_text("treaty.log")
    rows = len(back.split("\n")) - 1
    print(f"treaty.log: {rows} entries, {group3(len(back))} bytes, fnv {text_sum(back):08x}")

    for b in branches:
        os.remove(f"{b.name}_policies.csv")
        os.remove(f"{b.name}_claims.csv")
    os.remove("harbor_claims.bad")
    os.remove("treaty.log")


if __name__ == "__main__":
    try:
        run()
    except BatchError as e:
        print(f"claim_batch failed: {e}")
