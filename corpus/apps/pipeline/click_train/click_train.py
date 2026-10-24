"""Python mirror of click_train.kara, statement for statement. Integer
arithmetic only; tdiv is Kara's `/`, which truncates toward zero.

Run: python3 -I -B click_train.py > expected.out"""
import os

PER_DAY = 4000
N_USERS = 3000
N_ADS = 40
BUCKETS = 4096
BIAS = 4096
ONE = 65536
LR_DIV = 32
ZMAX = 524288


def tdiv(a, b):
    q = abs(a) // abs(b)
    return q if (a < 0) == (b < 0) else -q


class Imp:
    def __init__(self, ts, user, ad, hour, device, country, clicked):
        self.ts, self.user, self.ad, self.hour = ts, user, ad, hour
        self.device, self.country, self.clicked = device, country, clicked


class Day:
    def __init__(self, name, imps):
        self.name, self.imps = name, imps


class JobError(Exception):
    pass


def device_name(d):
    return "desktop" if d == 0 else "mobile" if d == 1 else "tablet"


def country_name(c):
    return ["US", "IN", "DE", "BR", "JP"][c] if c < 4 else "JP"


def lcg(state):
    return (state * 1103515245 + 12345) % 2147483648


def true_rate(imp):
    p = 150 + (imp.ad * 37 % 11) * 40
    if imp.device == 1:
        p += 60
    elif imp.device == 2:
        p -= 40
    if 18 <= imp.hour <= 22:
        p += 80
    if imp.country == 4:
        p += 50
    if imp.user % 7 == imp.ad % 7:
        p += 200
    return p


def gen_day(day):
    s = 1000003 * day + 12345
    out = ["ts,user,ad,hour,device,country,clicked\n"]
    for i in range(PER_DAY):
        ts = 1761264000 + day * 86400 + tdiv(i * 86400, PER_DAY)
        hour = tdiv(i * 24, PER_DAY)
        s = lcg(s)
        user = tdiv(s, 16) % N_USERS
        s = lcg(s)
        r = tdiv(s, 16) % 1000
        ad = tdiv(r * r, 25000)
        s = lcg(s)
        dv = tdiv(s, 16) % 100
        device = 1 if dv < 55 else 0 if dv < 90 else 2
        s = lcg(s)
        country = tdiv(s, 16) % 5
        imp = Imp(ts, user, ad, hour, device, country, 0)
        s = lcg(s)
        if tdiv(s, 16) % 10000 < true_rate(imp):
            imp.clicked = 1
        out.append(f"{ts},{user},{ad},{hour},{device_name(device)},{country_name(country)},{imp.clicked}\n")
    return "".join(out)


def parse_num(field, what, name, line):
    try:
        return int(field)
    except ValueError:
        raise JobError(f"{name} line {line}: bad {what} {field}")


def parse_day(name, data):
    try:
        text = data.decode("utf-8")
    except UnicodeDecodeError:
        raise JobError(f"{name}: not UTF-8")
    imps = []
    line_no = 0
    lines = text.split("\n")
    if lines and lines[-1] == "":
        lines.pop()
    for line in lines:
        line_no += 1
        if line_no == 1:
            if line != "ts,user,ad,hour,device,country,clicked":
                raise JobError(f"{name}: unexpected header {line}")
            continue
        f = line.split(",")
        if len(f) != 7:
            raise JobError(f"{name} line {line_no}: expected 7 fields, got {len(f)}")
        if f[4] == "desktop":
            device = 0
        elif f[4] == "mobile":
            device = 1
        elif f[4] == "tablet":
            device = 2
        else:
            raise JobError(f"{name} line {line_no}: unknown device {f[4]}")
        country = -1
        for c in range(5):
            if f[5] == country_name(c):
                country = c
        if country < 0:
            raise JobError(f"{name} line {line_no}: unknown country {f[5]}")
        imps.append(Imp(parse_num(f[0], "ts", name, line_no), parse_num(f[1], "user", name, line_no),
                        parse_num(f[2], "ad", name, line_no), parse_num(f[3], "hour", name, line_no),
                        device, country, parse_num(f[6], "clicked", name, line_no)))
    return Day(name, imps)


def mix32(x):
    h = x % 4294967296
    h = ((tdiv(h, 65536)) ^ h) * 73244475 % 4294967296
    h = ((tdiv(h, 65536)) ^ h) * 73244475 % 4294967296
    return tdiv(h, 65536) ^ h


def bucket(field, value):
    return mix32(field * 1000003 + value) % BUCKETS


def featurize(d):
    out = []
    for imp in d.imps:
        out.append(BIAS)
        out.append(bucket(1, imp.user))
        out.append(bucket(2, imp.ad))
        out.append(bucket(3, imp.hour))
        out.append(bucket(4, imp.device))
        out.append(bucket(5, imp.country))
        out.append(bucket(6, imp.ad * 8 + imp.device))
        out.append(bucket(7, imp.ad * 24 + imp.hour))
    return out


def pct(num, den):
    if den == 0:
        return "-"
    bp = tdiv(num * 10000, den)
    frac = bp % 100
    pad = "0" if frac < 10 else ""
    return f"{tdiv(bp, 100)}.{pad}{frac}%"


def ctr_by_device(d):
    out = f"{d.name} by device:"
    for dev in range(3):
        n, c = 0, 0
        for imp in d.imps:
            if imp.device == dev:
                n += 1
                c += imp.clicked
        out += f" {device_name(dev)} {pct(c, n)} of {n}"
    return out


def ctr_by_hour(d):
    marks = list(" .:-=+*#")
    bar = ""
    for h in range(24):
        n, c = 0, 0
        for imp in d.imps:
            if imp.hour == h:
                n += 1
                c += imp.clicked
        level = 0 if n == 0 else tdiv(tdiv(c * 1000, n), 8)
        bar += marks[7 if level > 7 else level]
    return f"{d.name} by hour: [{bar}]"


def top_ads(d):
    clicks = []
    for ad in range(N_ADS):
        c = 0
        for imp in d.imps:
            if imp.ad == ad:
                c += imp.clicked
        clicks.append((-c, ad))
    clicks.sort()
    out = f"{d.name} top ads:"
    for i in range(5):
        nc, ad = clicks[i]
        out += f" ad{ad}={-nc}"
    return out


def market_mix(d):
    out = f"{d.name} markets:"
    for c in range(5):
        n = 0
        for imp in d.imps:
            if imp.country == c:
                n += 1
        out += f" {country_name(c)} {pct(n, len(d.imps))}"
    return out


def bucket_load(name, feats):
    load = [0] * (BUCKETS + 1)
    for f in feats:
        load[f] += 1
    used, top, top_at = 0, 0, 0
    for b in range(BUCKETS):
        if load[b] > 0:
            used += 1
        if load[b] > top:
            top = load[b]
            top_at = b
    return f"{name} features: {len(feats)} hashed into {used} of {BUCKETS} buckets, fullest {top_at} with {top}"


def predict(w, feats, at):
    z = 0
    for k in range(8):
        z += w[feats[at * 8 + k]]
    return sigmoid(z)


SIG = [22, 36, 60, 98, 162, 267, 439, 720, 1179, 1921, 3108, 4971, 7812, 11955, 17625, 24743, 32768,
       40793, 47911, 53581, 57724, 60565, 62428, 63615, 64357, 64816, 65097, 65269, 65374, 65438, 65476,
       65500, 65514]


def sigmoid(z):
    t = SIG
    if z <= -ZMAX:
        return t[0]
    if z >= ZMAX:
        return t[32]
    u = z + ZMAX
    i = tdiv(u, ONE // 2)
    f = u % (ONE // 2)
    return t[i] + tdiv((t[i + 1] - t[i]) * f, ONE // 2)


def frac4(v):
    pad = "000" if v < 10 else "00" if v < 100 else "0" if v < 1000 else ""
    return f"0.{pad}{v}"


def brier(total, n):
    return frac4(tdiv(total * 10000, n * ONE))


def train_day(w, d, feats):
    sq, clicks, predicted = 0, 0, 0
    for i in range(len(d.imps)):
        p = predict(w, feats, i)
        y = d.imps[i].clicked * ONE
        err = y - p
        sq += tdiv(err * err, ONE)
        clicks += d.imps[i].clicked
        predicted += p
        step = tdiv(err, LR_DIV)
        for k in range(8):
            w[feats[i * 8 + k]] += step
    n = len(d.imps)
    return (f"train {d.name}: {n} impressions, {clicks} clicks, mean prediction "
            f"{pct(tdiv(predicted, ONE), n)}, progressive brier {brier(sq, n)}")


def auc(scores):
    s = sorted(scores)
    pos, neg, num = 0, 0, 0
    i = 0
    below = 0
    while i < len(s):
        j = i
        tp, tn = 0, 0
        while j < len(s) and s[j][0] == s[i][0]:
            if s[j][1] == 1:
                tp += 1
            else:
                tn += 1
            j += 1
        num += tp * (2 * below + tn)
        below += tn
        pos += tp
        neg += tn
        i = j
    return f"auc {frac4(tdiv(num * 10000, 2 * pos * neg))}"


def evaluate(w, d, feats):
    lines = []
    scores = []
    sq = 0
    for i in range(len(d.imps)):
        p = predict(w, feats, i)
        err = d.imps[i].clicked * ONE - p
        sq += tdiv(err * err, ONE)
        scores.append((p, d.imps[i].clicked))
    lines.append(f"holdout {d.name}: brier {brier(sq, len(d.imps))}, {auc(scores)}")
    edges = [0, 1638, 2621, 3604, 4915, 65537]
    for b in range(5):
        n, c, ps = 0, 0, 0
        for p, y in scores:
            if edges[b] <= p < edges[b + 1]:
                n += 1
                c += y
                ps += p
        lines.append(f"  band {b}: {n} impressions, predicted {pct(tdiv(ps, ONE), n)}, actual {pct(c, n)}")
    return lines


def top_weights(w):
    ws = sorted((-w[b], b) for b in range(BUCKETS))
    out = f"bias {w[BIAS]}, strongest buckets:"
    for i in range(5):
        nw, b = ws[i]
        out += f" {b}={-nw}"
    return out


def crc_table():
    t = []
    for n in range(256):
        c = n
        for _ in range(8):
            c = 3988292384 ^ tdiv(c, 2) if c % 2 == 1 else tdiv(c, 2)
        t.append(c)
    return t


def crc32(t, d):
    c = 4294967295
    for b in d:
        c = t[(c ^ b) % 256] ^ tdiv(c, 256)
    return c ^ 4294967295


def file_line(name, t, d):
    return f"{name}: {len(d)} bytes, crc32 {crc32(t, d):08x}"


def save_model(w):
    out = bytearray()
    for v in w:
        u = v + 4294967296 if v < 0 else v
        out.append(tdiv(u, 16777216))
        out.append(tdiv(u, 65536) % 256)
        out.append(tdiv(u, 256) % 256)
        out.append(u % 256)
    return bytes(out)


def load_model(b):
    w = []
    i = 0
    while i + 4 <= len(b):
        u = b[i] * 16777216 + b[i + 1] * 65536 + b[i + 2] * 256 + b[i + 3]
        w.append(u - 4294967296 if u >= 2147483648 else u)
        i += 4
    return w


def read_file(path):
    try:
        with open(path, "rb") as f:
            return f.read()
    except OSError:
        raise JobError(f"could not read {path}")


def write_bytes(path, data):
    with open(path, "wb") as f:
        f.write(data)


def run():
    tc = crc_table()
    csv1, csv2, csv3, csv4 = gen_day(1), gen_day(2), gen_day(3), gen_day(4)
    write_bytes("day1.csv", csv1.encode())
    write_bytes("day2.csv", csv2.encode())
    write_bytes("day3.csv", csv3.encode())
    write_bytes("day4.csv", csv4.encode())
    whole = read_file("day4.csv")
    write_bytes("day4_cut.csv", whole[0:5000])
    b1, b2, b3, b4 = (read_file(f"day{k}.csv") for k in (1, 2, 3, 4))
    print(file_line("day1.csv", tc, b1))
    print(file_line("day2.csv", tc, b2))
    print(file_line("day3.csv", tc, b3))
    print(file_line("day4.csv", tc, b4))
    d1 = parse_day("day1", b1)
    d2 = parse_day("day2", b2)
    d3 = parse_day("day3", b3)
    d4 = parse_day("day4", b4)
    try:
        d = parse_day("day4_cut", read_file("day4_cut.csv"))
        print(f"day4_cut: parsed {len(d.imps)} impressions")
    except JobError as e:
        print(e)
    f1, f2, f3, f4 = featurize(d1), featurize(d2), featurize(d3), featurize(d4)
    days = [d1, d2, d3, d4]
    feats = [f1, f2, f3, f4]
    lines = ([ctr_by_device(d) for d in days] + [ctr_by_hour(d) for d in days] + [top_ads(d) for d in days]
             + [market_mix(d) for d in days] + [bucket_load(d.name, f) for d, f in zip(days, feats)])
    for line in lines:
        print(line)
    w = [0] * (BUCKETS + 1)
    for line in [train_day(w, d, f) for d, f in zip(days, feats)]:
        print(line)
    print(top_weights(w))
    hold = parse_day("day5", gen_day(5).encode())
    hf = featurize(hold)
    for line in evaluate(w, hold, hf):
        print(line)
    write_bytes("model.bin", save_model(w))
    back = load_model(read_file("model.bin"))
    print(file_line("model.bin", tc, read_file("model.bin")))
    same = back == w
    print(f"reloaded {len(back)} weights, identical: {str(same).lower()}")
    for line in evaluate(back, hold, hf):
        print(line)
    for p in ["day1.csv", "day2.csv", "day3.csv", "day4.csv", "day4_cut.csv", "model.bin"]:
        try:
            os.remove(p)
        except OSError:
            pass


def main():
    try:
        run()
    except JobError as e:
        print(f"click_train failed: {e}")


main()
