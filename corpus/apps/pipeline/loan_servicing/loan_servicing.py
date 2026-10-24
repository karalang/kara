"""pipeline/loan_servicing, the Python mirror: a lender's October 2026
servicing month over four books of loans (mortgage, auto, student and
personal), from each book's loan tape and payment file to the posting
that applies every payment through the servicing waterfall, the
per-book reports, and one collections desk whose call slots every book's
delinquent accounts draw on. It wrote expected.out.

Run from this directory: python3 -I -B loan_servicing.py > expected.out
LOAN_REVERSE=1 queues the books at the desk in reverse order."""
import os

MASK32 = 4294967295
# Day 0 is Thursday 1 October 2026.
MONTH_START = [0, 31, 59, 90, 120, 151, 181, 212, 243, 273, 304, 334]
MONTH_DAYS = [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
DAY0 = 273
LAST_DAY = 30
STATES = ["CA", "TX", "FL", "NY", "PA", "IL", "OH", "GA", "NC", "MI", "WA", "AZ"]
CHANNELS = ["ACH", "CHK", "CARD", "WIRE"]
# By kind of loan: mortgage, auto, student, personal.
BAL_LO = [8000000, 500000, 300000, 100000]
BAL_HI = [60000000, 4500000, 9000000, 3000000]
RATE_LO = [300, 400, 450, 800]
RATE_HI = [750, 1800, 850, 2900]
TERMS = [[360, 180], [36, 48, 60, 72], [120], [24, 36, 48, 60]]
FEE_CAP = [10000, 3500, 2500, 3900]
MAX_K = [5, 4, 9, 5]
ACTION_K = [4, 4, 9, 5]
ACTION_NAMES = ["referred to foreclosure", "assigned for repossession", "defaulted", "charged off"]
BUCKETS = ["current", "30", "60", "90", "120+"]
FEE_PM = 500
NSF_FEE = 2500
GRACE = 15
# The collections desk: the business days of 2 to 13 November, each with
# the same number of call slots.
DESK_DAYS = [32, 33, 34, 35, 36, 39, 40, 41, 42, 43]
DAY_CAP = 80
MAX_CALLS = 3

LOAN_HEADER = "loan,state,age,term,rate,balance,payment,escrow,due,past_due,late_fees,suspense,autopay"
PAY_HEADER = "payment,day,loan,amount,channel,ref"


class BatchError(Exception):
    pass


class Book:
    def __init__(self, name, code, kind, seed, count, strangers):
        self.name, self.code, self.kind, self.seed = name, code, kind, seed
        self.count, self.strangers = count, strangers


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
    a = abs(c)
    return "%s$%s.%02d" % (sign, group3(a // 100), a % 100)


def dollars(c):
    return "%d.%02d" % (c // 100, c % 100)


def ceil_div(a, b):
    return (a + b - 1) // b


def date_of(day):
    n = DAY0 + day
    m = 11
    while MONTH_START[m] > n:
        m -= 1
    return "2026-%02d-%02d" % (m + 1, n - MONTH_START[m] + 1)


def day_of(t):
    if len(t) != 10 or t[4] != "-" or t[7] != "-":
        return None
    ys, ms, ds = t[0:4], t[5:7], t[8:10]
    if not (ys.isdigit() and ms.isdigit() and ds.isdigit()):
        return None
    y, m, d = int(ys), int(ms), int(ds)
    if y != 2026 or m < 1 or m > 12 or d < 1 or d > MONTH_DAYS[m - 1]:
        return None
    return MONTH_START[m - 1] + d - 1 - DAY0


def short_date(day):
    return date_of(day)[5:10]


def interest_of(bal, rate):
    return (bal * rate + 60000) // 120000


def late_fee(payment, kind):
    return min((payment * FEE_PM + 5000) // 10000, FEE_CAP[kind])


def loan_id(book, n):
    return "%s%05d" % (book.code, n)


# ------------------------------------------------------------ building

class Loan:
    FIELDS = ["loan", "state", "age", "term", "rate", "balance", "payment", "escrow", "due",
              "past_due", "late_fees", "suspense", "autopay"]

    def __init__(self, **kw):
        for f in self.FIELDS:
            setattr(self, f, kw[f])

    def key(self):
        return tuple(getattr(self, f) for f in self.FIELDS)


class Pay:
    def __init__(self, pid, day, loan, amount, channel, ref):
        self.pid, self.day, self.loan, self.amount = pid, day, loan, amount
        self.channel, self.ref = channel, ref

    def key(self):
        return (self.pid, self.day, self.loan, self.amount, self.channel, self.ref)


def build_loans(book):
    s, kind = book.seed, book.kind
    out = []
    for i in range(book.count):
        opened = BAL_LO[kind] + mix(s, i, 1) % (BAL_HI[kind] - BAL_LO[kind])
        rate = RATE_LO[kind] + mix(s, i, 2) % (RATE_HI[kind] - RATE_LO[kind] + 1)
        rate -= rate % 5
        terms = TERMS[kind]
        term = terms[mix(s, i, 3) % len(terms)]
        age = 1 + mix(s, i, 4) % (term - 1)
        bal = max(opened * (term - age) // term, 50000)
        payment = interest_of(bal, rate) + ceil_div(bal, term - age)
        escrow = 15000 + mix(s, i, 5) % 75001 if kind == 0 else 0
        due = 1 + mix(s, i, 6) % 28
        state = STATES[mix(s, i, 7) % len(STATES)]
        r = mix(s, i, 8) % 100
        if r < 78:
            k = 0
        elif r < 88:
            k = 1
        elif r < 93:
            k = 2
        elif r < 96:
            k = 3
        else:
            k = 4 + mix(s, i, 9) % (MAX_K[kind] - 3)
        late = late_fee(payment, kind) if k > 0 else 0
        suspense = 0
        if k > 0 and mix(s, i, 10) % 3 == 0:
            suspense = (payment + escrow) // (2 + mix(s, i, 11) % 3)
        autopay = k == 0 and mix(s, i, 12) % 100 < 40
        out.append(Loan(loan=loan_id(book, i + 1), state=state, age=age, term=term, rate=rate,
                        balance=bal, payment=payment, escrow=escrow, due=due, past_due=k,
                        late_fees=late, suspense=suspense, autopay=autopay))
    return out


def build_payments(book, loans):
    s = book.seed
    raw = []  # [day, order, loan, amount, channel, reverses order or -1]

    def add(day, loan, amount, channel, rev):
        raw.append([day, len(raw), loan, amount, channel, rev])
        return len(raw) - 1

    for i, ln in enumerate(loans):
        inst = ln.payment + ln.escrow
        due = ln.due - 1
        b = mix(s, i, 20) % 100
        channel = CHANNELS[mix(s, i, 21) % 4]
        day = min(max(due + mix(s, i, 22) % 26 - 5, 0), LAST_DAY)
        first = -1
        if ln.autopay:
            first = add(due, ln.loan, inst, "ACH", -1)
        elif ln.past_due == 0:
            if b < 72:
                first = add(day, ln.loan, inst, channel, -1)
            elif b < 78:
                first = add(day, ln.loan, inst + 10000 + mix(s, i, 23) % 190001, channel, -1)
            elif b < 81:
                quote = ln.balance + interest_of(ln.balance, ln.rate) + ln.escrow + mix(s, i, 24) % 5000
                first = add(day, ln.loan, quote, "WIRE", -1)
            elif b < 88:
                half = inst // 2
                first = add(day, ln.loan, half, channel, -1)
                add(min(day + 7, LAST_DAY), ln.loan, inst - half, channel, -1)
            elif b < 95:
                pass
            else:
                first = add(day, ln.loan, inst // 2, channel, -1)
        else:
            owed = ln.past_due + 1
            if b < 35:
                pass
            elif b < 65:
                first = add(day, ln.loan, inst, channel, -1)
            elif b < 80:
                first = add(day, ln.loan, ln.late_fees + owed * inst - ln.suspense, channel, -1)
            else:
                first = add(day, ln.loan, inst // 2, channel, -1)
        if first >= 0 and raw[first][4] == "CHK" and mix(s, i, 30) % 100 < 6:
            back = min(raw[first][0] + 2 + mix(s, i, 31) % 5, LAST_DAY)
            add(back, ln.loan, raw[first][3], "NSF", first)
    for j in range(book.strangers):
        add(mix(s, j, 40) % (LAST_DAY + 1), loan_id(book, book.count + 101 + j * 7),
            5000 + mix(s, j, 41) % 200000, "CHK", -1)
    ids = {}
    out = []
    np = nr = 0
    for day in range(LAST_DAY + 1):
        for r in raw:
            if r[0] != day:
                continue
            if r[4] == "NSF":
                nr += 1
                pid = "%sR%05d" % (book.code, nr)
                ref = ids[r[5]]
            else:
                np += 1
                pid = "%sP%05d" % (book.code, np)
                ref = "-"
            ids[r[1]] = pid
            out.append(Pay(pid, day, r[2], r[3], r[4], ref))
    return out


def encode_loans(loans):
    out = [LOAN_HEADER]
    for ln in loans:
        out.append("%s,%s,%d,%d,%d,%s,%s,%s,%d,%d,%s,%s,%s" % (
            ln.loan, ln.state, ln.age, ln.term, ln.rate, dollars(ln.balance), dollars(ln.payment),
            dollars(ln.escrow), ln.due, ln.past_due, dollars(ln.late_fees), dollars(ln.suspense),
            "Y" if ln.autopay else "N"))
    return "\n".join(out) + "\n"


def encode_payments(pays):
    out = [PAY_HEADER]
    for p in pays:
        out.append("%s,%s,%s,%s,%s,%s" % (p.pid, date_of(p.day), p.loan, dollars(p.amount), p.channel, p.ref))
    return "\n".join(out) + "\n"


def damage(text, n, field, value):
    lines = text.split("\n")
    f = lines[n].split(",")
    f[field] = value
    lines[n] = ",".join(f)
    return "\n".join(lines)


# ------------------------------------------------------------ parsing

def is_digits(s):
    return len(s) > 0 and all("0" <= c <= "9" for c in s)


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


def body_lines(path, text, header):
    lines = text.split("\n")
    if len(lines) < 2 or lines[-1] != "":
        raise BatchError("%s: no final newline" % path)
    if lines[0] != header:
        raise BatchError("%s line 1: bad header" % path)
    return lines[1:-1]


def int_field(path, n, what, t, lo, hi):
    v = small_int(t, lo, hi)
    if v is None:
        raise BatchError("%s line %d: bad %s '%s'" % (path, n, what, t))
    return v


def cents_field(path, n, what, t):
    v = cents_of(t)
    if v is None:
        raise BatchError("%s line %d: bad %s '%s'" % (path, n, what, t))
    return v


def id_ok(t, prefix, digits):
    return len(t) == len(prefix) + digits and t.startswith(prefix) and is_digits(t[len(prefix):])


def parse_loans(path, book, text):
    out = []
    seen = set()
    for i, line in enumerate(body_lines(path, text, LOAN_HEADER)):
        n = i + 2
        f = line.split(",")
        if len(f) != 13:
            raise BatchError("%s line %d: %d fields, not 13" % (path, n, len(f)))
        if not id_ok(f[0], f[0][:1], 5) or f[0][:1] < "A" or f[0][:1] > "Z":
            raise BatchError("%s line %d: bad loan '%s'" % (path, n, f[0]))
        if f[0][:1] != book.code:
            raise BatchError("%s line %d: loan '%s' is not %s's" % (path, n, f[0], book.name))
        if f[0] in seen:
            raise BatchError("%s line %d: loan '%s' twice" % (path, n, f[0]))
        seen.add(f[0])
        if f[1] not in STATES:
            raise BatchError("%s line %d: bad state '%s'" % (path, n, f[1]))
        term = int_field(path, n, "term", f[3], 1, 600)
        if term not in TERMS[book.kind]:
            raise BatchError("%s line %d: no %s loan has a %d-month term" % (path, n, book.name, term))
        age = int_field(path, n, "age", f[2], 1, term - 1)
        rate = int_field(path, n, "rate", f[4], RATE_LO[book.kind], RATE_HI[book.kind])
        bal = cents_field(path, n, "balance", f[5])
        payment = cents_field(path, n, "payment", f[6])
        escrow = cents_field(path, n, "escrow", f[7])
        if escrow != 0 and book.kind != 0:
            raise BatchError("%s line %d: escrow on a %s loan" % (path, n, book.name))
        due = int_field(path, n, "due day", f[8], 1, 28)
        k = int_field(path, n, "past due", f[9], 0, 12)
        late = cents_field(path, n, "late fees", f[10])
        suspense = cents_field(path, n, "suspense", f[11])
        if f[12] not in ("Y", "N"):
            raise BatchError("%s line %d: bad autopay '%s'" % (path, n, f[12]))
        out.append(Loan(loan=f[0], state=f[1], age=age, term=term, rate=rate, balance=bal,
                        payment=payment, escrow=escrow, due=due, past_due=k, late_fees=late,
                        suspense=suspense, autopay=f[12] == "Y"))
    return out


def parse_payments(path, book, text):
    out = []
    seen = {}
    last = 0
    for i, line in enumerate(body_lines(path, text, PAY_HEADER)):
        n = i + 2
        f = line.split(",")
        if len(f) != 6:
            raise BatchError("%s line %d: %d fields, not 6" % (path, n, len(f)))
        if not (id_ok(f[0], book.code + "P", 5) or id_ok(f[0], book.code + "R", 5)):
            raise BatchError("%s line %d: payment '%s' is not %s's" % (path, n, f[0], book.name))
        if f[0] in seen:
            raise BatchError("%s line %d: payment '%s' twice" % (path, n, f[0]))
        day = day_of(f[1])
        if day is None or day < 0 or day > LAST_DAY:
            raise BatchError("%s line %d: bad date '%s'" % (path, n, f[1]))
        if day < last:
            raise BatchError("%s line %d: date %s is before the line above" % (path, n, f[1]))
        last = day
        if not id_ok(f[2], book.code, 5):
            raise BatchError("%s line %d: bad loan '%s'" % (path, n, f[2]))
        amount = cents_field(path, n, "amount", f[3])
        if amount == 0:
            raise BatchError("%s line %d: zero amount" % (path, n))
        if f[4] == "NSF":
            if f[0][1] != "R":
                raise BatchError("%s line %d: a return numbered as a payment" % (path, n))
            if f[5] not in seen:
                raise BatchError("%s line %d: returns '%s', which is not above" % (path, n, f[5]))
            orig = out[seen[f[5]]]
            if orig.loan != f[2] or orig.amount != amount or orig.channel != "CHK":
                raise BatchError("%s line %d: does not match the check it returns" % (path, n))
        else:
            if f[4] not in CHANNELS:
                raise BatchError("%s line %d: bad channel '%s'" % (path, n, f[4]))
            if f[0][1] != "P" or f[5] != "-":
                raise BatchError("%s line %d: a payment numbered or referenced as a return" % (path, n))
        seen[f[0]] = len(out)
        out.append(Pay(f[0], day, f[2], amount, f[4], f[5]))
    return out


# ------------------------------------------------------------ posting

class Post:
    """What posting a book's month left behind."""

    def __init__(self, loans):
        self.bal = [ln.balance for ln in loans]
        self.owed = [ln.past_due + 1 for ln in loans]
        self.late = [ln.late_fees for ln in loans]
        self.susp = [ln.suspense for ln in loans]
        self.fee_done = [False] * len(loans)
        # 0 open, 1 paid off, 2 the book's action
        self.status = [0] * len(loans)
        self.ch_n = [0] * 4
        self.ch_amt = [0] * 4
        self.nsf_n = self.nsf_amt = 0
        self.unk_n = self.unk_amt = 0
        self.ref_n = self.ref_amt = 0
        self.fee_paid = self.int_paid = self.prin_paid = self.esc_paid = self.curtail = 0
        self.fees_n = self.fees_amt = 0
        self.nsf_fees = 0


def assess(post, loans, kind, j):
    fee = late_fee(loans[j].payment, kind)
    post.late[j] += fee
    post.fee_done[j] = True
    post.fees_n += 1
    post.fees_amt += fee


def post_book(book, loans, pays):
    post = Post(loans)
    index = {ln.loan: j for j, ln in enumerate(loans)}
    returned = set(p.ref for p in pays if p.channel == "NSF")
    for p in pays:
        j = index.get(p.loan)
        if p.channel == "NSF":
            if j is not None and post.status[j] == 0:
                post.late[j] += NSF_FEE
                post.nsf_fees += 1
            continue
        c = CHANNELS.index(p.channel)
        post.ch_n[c] += 1
        post.ch_amt[c] += p.amount
        if p.pid in returned:
            post.nsf_n += 1
            post.nsf_amt += p.amount
            continue
        if j is None:
            post.unk_n += 1
            post.unk_amt += p.amount
            continue
        if post.status[j] != 0:
            post.ref_n += 1
            post.ref_amt += p.amount
            continue
        ln = loans[j]
        if not post.fee_done[j] and post.owed[j] > 0 and p.day > ln.due - 1 + GRACE:
            assess(post, loans, book.kind, j)
        funds = p.amount + post.susp[j]
        post.susp[j] = 0
        f = min(funds, post.late[j])
        post.late[j] -= f
        post.fee_paid += f
        funds -= f
        inst = ln.payment + ln.escrow
        while post.owed[j] > 0 and funds >= inst and post.bal[j] > 0:
            interest = interest_of(post.bal[j], ln.rate)
            principal = min(max(ln.payment - interest, 0), post.bal[j])
            post.bal[j] -= principal
            post.int_paid += interest
            post.prin_paid += principal
            post.esc_paid += ln.escrow
            funds -= inst
            post.owed[j] -= 1
        if funds > 0 and post.owed[j] == 0:
            if funds >= post.bal[j]:
                post.curtail += post.bal[j]
                if funds > post.bal[j]:
                    post.ref_n += 1
                    post.ref_amt += funds - post.bal[j]
                post.bal[j] = 0
            else:
                post.curtail += funds
                post.bal[j] -= funds
            funds = 0
        if post.bal[j] == 0:
            post.status[j] = 1
            post.owed[j] = 0
            if funds > 0:
                post.ref_n += 1
                post.ref_amt += funds
            funds = 0
        post.susp[j] = funds
    for j, ln in enumerate(loans):
        if post.status[j] != 0:
            continue
        if not post.fee_done[j] and post.owed[j] > 0 and LAST_DAY > ln.due - 1 + GRACE:
            assess(post, loans, book.kind, j)
        if post.owed[j] >= ACTION_K[book.kind]:
            post.status[j] = 2
    return post


# ------------------------------------------------------------ reports

def cash_report(book, post):
    n = sum(post.ch_n)
    total = sum(post.ch_amt)
    chans = ", ".join("%s %d %s" % (CHANNELS[c], post.ch_n[c], money(post.ch_amt[c])) for c in range(4))
    held = [s for s in post.susp if s > 0]
    return ["%s cash: %s payments, %s (%s); %d returned, %s; %d unidentified, %s; %d refunds, %s" % (
                book.name, group3(n), money(total), chans, post.nsf_n, money(post.nsf_amt), post.unk_n,
                money(post.unk_amt), post.ref_n, money(post.ref_amt)),
            "  applied to fees %s, interest %s, principal %s, escrow %s, curtailments %s; %d loans hold %s in suspense" % (
                money(post.fee_paid), money(post.int_paid), money(post.prin_paid), money(post.esc_paid),
                money(post.curtail), len(held), money(sum(held)))]


def end_bucket(post, j):
    if post.status[j] == 1:
        return 5
    return min(post.owed[j], 4)


def roll_report(book, loans, post):
    grid = [[0] * 6 for _ in range(5)]
    cured = worse = same = 0
    for j, ln in enumerate(loans):
        a = min(ln.past_due, 4)
        b = end_bucket(post, j)
        grid[a][b] += 1
        if b == 5:
            continue
        if a > 0 and b == 0:
            cured += 1
        elif b > a:
            worse += 1
        elif b == a:
            same += 1
    names = BUCKETS + ["paid off"]
    paid = sum(grid[a][5] for a in range(5))
    out = ["%s roll: %d cured, %d rolled worse, %d stayed, %d paid off" % (book.name, cured, worse, same, paid)]
    for a in range(5):
        if sum(grid[a]) == 0:
            continue
        cells = ", ".join("%s %d" % (names[b], grid[a][b]) for b in range(6) if grid[a][b] > 0)
        out.append("  %s: %d -> %s" % (BUCKETS[a], sum(grid[a]), cells))
    return out


def on_book(book, post, j):
    return post.status[j] == 0 or (post.status[j] == 2 and book.kind != 3)


def balance_report(book, loans, post):
    n = tot = weighted = 0
    by_state = {}
    for j, ln in enumerate(loans):
        if not on_book(book, post, j):
            continue
        n += 1
        tot += post.bal[j]
        weighted += post.bal[j] * ln.rate
        s = by_state.setdefault(ln.state, [0, 0])
        s[0] += post.bal[j]
        s[1] += 1
    w = (weighted * 2 + tot) // (2 * tot)
    top = sorted(by_state.items(), key=lambda kv: (-kv[1][0], kv[0]))[:3]
    paid = sum(1 for s in post.status if s == 1)
    return "%s book: %s loans, %s, %d.%02d%% weighted rate; %d paid off; top states %s" % (
        book.name, group3(n), money(tot), w // 100, w % 100, paid,
        ", ".join("%s %s (%d)" % (k, money(v[0]), v[1]) for k, v in top))


def action_report(book, loans, post):
    acts = [j for j in range(len(loans)) if post.status[j] == 2]
    bal = sum(post.bal[j] for j in acts)
    line = "%s actions: %d %s (%s); %d late fees, %s; %d NSF fees" % (
        book.name, len(acts), ACTION_NAMES[book.kind], money(bal), post.fees_n, money(post.fees_amt),
        post.nsf_fees)
    if acts:
        j = acts[0]
        line += "; first %s, %d days past due, %s" % (loans[j].loan, post.owed[j] * 30, money(post.bal[j]))
    return line


# ------------------------------------------------------------ the desk

class Desk:
    def __init__(self):
        self.left = [DAY_CAP] * len(DESK_DAYS)
        self.log = []


def queue_book(desk, book, loans, post):
    queue = [j for j in range(len(loans)) if post.status[j] == 0 and post.owed[j] > 0]
    queue.sort(key=lambda j: (-post.owed[j], -post.bal[j], loans[j].loan))
    full = part = none = 0
    bal = none_bal = 0
    for j in queue:
        need = min(post.owed[j], MAX_CALLS)
        bal += post.bal[j]
        placed = 0
        d = 0
        while placed < need:
            while d < len(DESK_DAYS) and desk.left[d] == 0:
                d += 1
            if d == len(DESK_DAYS):
                break
            desk.left[d] -= 1
            placed += 1
            desk.log.append("%s %s call %d of %d" % (date_of(DESK_DAYS[d]), loans[j].loan, placed, need))
            d += 1
        if placed == need:
            full += 1
        elif placed > 0:
            part += 1
        else:
            none += 1
            none_bal += post.bal[j]
    return "%s desk: %d accounts, %s: %d worked, %d part-worked, %d not reached (%s)" % (
        book.name, len(queue), money(bal), full, part, none, money(none_bal))


def desk_summary(desk):
    cap = DAY_CAP * len(DESK_DAYS)
    used = cap - sum(desk.left)
    days = ", ".join("%s %d" % (short_date(DESK_DAYS[d]), DAY_CAP - desk.left[d]) for d in range(len(DESK_DAYS)))
    return "desk: %d of %d calls booked; %s" % (used, cap, days)


def file_line(name, text):
    rows = len(text.split("\n")) - 2
    return "%s: %s rows, %s bytes, fnv %08x" % (name, group3(rows), group3(len(text)), text_sum(text))


def read_text(path):
    try:
        with open(path) as f:
            return f.read()
    except OSError:
        raise BatchError("cannot read %s" % path)


def write_text(path, text):
    with open(path, "w") as f:
        f.write(text)


def run():
    books = [Book("mortgage", "M", 0, 311, 400, 4), Book("auto", "A", 1, 422, 700, 6),
             Book("student", "S", 2, 533, 500, 5), Book("personal", "P", 3, 644, 600, 7)]
    loans = [build_loans(b) for b in books]
    pays = [build_payments(b, ls) for b, ls in zip(books, loans)]
    for b, ls, ps in zip(books, loans, pays):
        write_text(b.name + "_loans.csv", encode_loans(ls))
        write_text(b.name + "_payments.csv", encode_payments(ps))

    # A copy of auto's payments with one amount cut short, and a copy of
    # student's with one payment dated before the line above it.
    whole = read_text("auto_payments.csv")
    amount = whole.split("\n")[90].split(",")[3]
    write_text("auto_payments.bad", damage(whole, 90, 3, amount[:-1]))
    whole2 = read_text("student_payments.csv")
    earlier = whole2.split("\n")[30].split(",")[1]
    write_text("student_payments.bad", damage(whole2, 250, 1, earlier))

    tl = [read_text(b.name + "_loans.csv") for b in books]
    tp = [read_text(b.name + "_payments.csv") for b in books]
    for b, t1, t2 in zip(books, tl, tp):
        print(file_line(b.name + " loans", t1))
        print(file_line(b.name + " payments", t2))

    gl = [parse_loans(b.name + "_loans.csv", b, t) for b, t in zip(books, tl)]
    gp = [parse_payments(b.name + "_payments.csv", b, t) for b, t in zip(books, tp)]
    for path, b, t in [("auto_payments.bad", books[1], read_text("auto_payments.bad")),
                       ("student_payments.bad", books[2], read_text("student_payments.bad"))]:
        try:
            parse_payments(path, b, t)
            print(path + ": parsed")
        except BatchError as e:
            print(e)
    try:
        parse_loans("personal_loans.csv", books[0], tl[3])
        print("personal_loans.csv: parsed")
    except BatchError as e:
        print(e)
    same = all([x.key() for x in a] == [x.key() for x in c] for a, c in zip(gl + gp, loans + pays))
    print("parsed files match the built ones: %s" % ("true" if same else "false"))

    posts = [post_book(b, ls, ps) for b, ls, ps in zip(books, gl, gp)]
    for b, p in zip(books, posts):
        for line in cash_report(b, p):
            print(line)
    for b, ls, p in zip(books, gl, posts):
        for line in roll_report(b, ls, p):
            print(line)
    for b, ls, p in zip(books, gl, posts):
        print(balance_report(b, ls, p))
    for b, ls, p in zip(books, gl, posts):
        print(action_report(b, ls, p))

    desk = Desk()
    order = [0, 1, 2, 3]
    if os.environ.get("LOAN_REVERSE") == "1":
        order.reverse()
    for k in order:
        print(queue_book(desk, books[k], gl[k], posts[k]))
    print(desk_summary(desk))
    write_text("collections.log", "\n".join(desk.log) + "\n")
    back = read_text("collections.log")
    print("collections.log: %s entries, %s bytes, fnv %08x" % (
        group3(len(back.split("\n")) - 1), group3(len(back)), text_sum(back)))
    for b in books:
        os.remove(b.name + "_loans.csv")
        os.remove(b.name + "_payments.csv")
    for p in ["auto_payments.bad", "student_payments.bad", "collections.log"]:
        os.remove(p)


def main():
    try:
        run()
    except BatchError as e:
        print("loan_servicing failed: %s" % e)


main()
