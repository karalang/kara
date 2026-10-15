"""Mirror of tar_gz.kara: a ustar archiver and a gzip/DEFLATE codec written
from the RFCs (1951, 1952) and POSIX ustar. The compressor is a port of the
Kara (same match finder, same Huffman builder), so sizes agree bit for bit;
the dates come from Python's datetime as an independent oracle."""
import datetime as dt
import os

MTIME = 1760000000


class Bad(Exception):
    pass


def crc_table():
    t = []
    for n in range(256):
        c = n
        for _ in range(8):
            c = 0xEDB88320 ^ (c >> 1) if c & 1 else c >> 1
        t.append(c)
    return t


TABLE = crc_table()


def crc32(data):
    c = 0xFFFFFFFF
    for b in data:
        c = TABLE[(c ^ b) & 0xFF] ^ (c >> 8)
    return c ^ 0xFFFFFFFF


def octal(v, width):
    s = ""
    while v > 0:
        s = str(v % 8) + s
        v //= 8
    return "0" * (width - len(s)) + s


# ---------------------------------------------------------------- tables

def length_tables():
    base, extra = [], []
    b = 3
    for code in range(28):
        e = 0 if code < 8 else (code - 4) // 4
        base.append(b)
        extra.append(e)
        b += 1 << e
    base.append(258)
    extra.append(0)
    return base, extra


def dist_tables():
    base, extra = [], []
    b = 1
    for code in range(30):
        e = 0 if code < 4 else (code - 2) // 2
        base.append(b)
        extra.append(e)
        b += 1 << e
    return base, extra


LBASE, LEXTRA = length_tables()
DBASE, DEXTRA = dist_tables()
CL_ORDER = [16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15]


def length_code(n):
    c = 0
    while c < 28 and LBASE[c + 1] <= n:
        c += 1
    return c


def dist_code(d):
    c = 0
    while c < 29 and DBASE[c + 1] <= d:
        c += 1
    return c


def fixed_lengths():
    lit = [8] * 144 + [9] * 112 + [7] * 24 + [8] * 8
    return lit, [5] * 30


# ---------------------------------------------------------------- compress

def lz77(d):
    n = len(d)
    head = [-1] * 32768
    prev = [-1] * n
    lits, dists = [], []

    def h(i):
        return ((d[i] << 10) ^ (d[i + 1] << 5) ^ d[i + 2]) & 0x7FFF

    def insert(p):
        if p + 2 < n:
            hv = h(p)
            prev[p] = head[hv]
            head[hv] = p

    i = 0
    while i < n:
        best, bdist = 0, 0
        if i + 2 < n:
            j = head[h(i)]
            chain = 0
            maxl = min(258, n - i)
            while j >= 0 and i - j <= 32768 and chain < 64:
                ln = 0
                while ln < maxl and d[j + ln] == d[i + ln]:
                    ln += 1
                if ln > best:
                    best, bdist = ln, i - j
                    if ln == maxl:
                        break
                j = prev[j]
                chain += 1
        if best >= 3:
            lits.append(best)
            dists.append(bdist)
            for p in range(i, i + best):
                insert(p)
            i += best
        else:
            lits.append(d[i])
            dists.append(0)
            insert(i)
            i += 1
    return lits, dists


def huffman(f):
    L = [0] * len(f)
    syms = [i for i in range(len(f)) if f[i] > 0]
    if not syms:
        return L
    if len(syms) == 1:
        L[syms[0]] = 1
        return L
    weight = [f[s] for s in syms]
    parent = [-1] * len(syms)
    active = list(range(len(syms)))

    def take():
        k = 0
        for x in range(1, len(active)):
            if weight[active[x]] < weight[active[k]]:
                k = x
        return active.pop(k)

    while len(active) > 1:
        a = take()
        b = take()
        weight.append(weight[a] + weight[b])
        parent.append(-1)
        parent[a] = parent[b] = len(weight) - 1
        active.append(len(weight) - 1)
    for k, s in enumerate(syms):
        depth, x = 0, k
        while parent[x] != -1:
            x = parent[x]
            depth += 1
        L[s] = depth
    return L


def limited(f, limit):
    f = list(f)
    rounds = 0
    while True:
        L = huffman(f)
        if max(L) <= limit:
            return L, rounds
        f = [(x + 1) // 2 for x in f]
        rounds += 1


def canonical(L):
    count = [0] * 16
    for x in L:
        if x:
            count[x] += 1
    nxt = [0] * 16
    code = 0
    for b in range(1, 16):
        code = (code + count[b - 1]) << 1 if b > 1 else 0
        nxt[b] = code
    out = [0] * len(L)
    for s, x in enumerate(L):
        if x:
            out[s] = nxt[x]
            nxt[x] += 1
    return out


class Bits:
    def __init__(self):
        self.out, self.acc, self.n = bytearray(), 0, 0

    def put(self, v, n):
        self.acc |= v << self.n
        self.n += n
        while self.n >= 8:
            self.out.append(self.acc & 0xFF)
            self.acc >>= 8
            self.n -= 8

    def huff(self, code, n):
        r = 0
        for _ in range(n):
            r = (r << 1) | (code & 1)
            code >>= 1
        self.put(r, n)

    def flush(self):
        if self.n > 0:
            self.out.append(self.acc & 0xFF)
        self.acc, self.n = 0, 0


def rle(seq):
    out = []
    i = 0
    while i < len(seq):
        v = seq[i]
        run = 1
        while i + run < len(seq) and seq[i + run] == v:
            run += 1
        if v == 0 and run >= 3:
            r = min(run, 138)
            out.append((18, r - 11) if r >= 11 else (17, r - 3))
            i += r
        elif v != 0 and run >= 4:
            out.append((v, 0))
            r = min(run - 1, 6)
            out.append((16, r - 3))
            i += 1 + r
        else:
            out.append((v, 0))
            i += 1
    return out


CL_EXTRA = {16: 2, 17: 3, 18: 7}


def data_bits(lits, dists, ll, dl):
    bits = ll[256]
    for v, d in zip(lits, dists):
        if d == 0:
            bits += ll[v]
        else:
            lc, dc = length_code(v), dist_code(d)
            bits += ll[257 + lc] + LEXTRA[lc] + dl[dc] + DEXTRA[dc]
    return bits


def deflate(d):
    lits, dists = lz77(d)
    lf, df = [0] * 286, [0] * 30
    for v, dd in zip(lits, dists):
        if dd == 0:
            lf[v] += 1
        else:
            lf[257 + length_code(v)] += 1
            df[dist_code(dd)] += 1
    lf[256] += 1
    if sum(1 for x in lf if x) < 2:
        lf[0] = max(lf[0], 1)
    if sum(1 for x in df if x) < 2:
        df[0] = max(df[0], 1)
        df[1] = max(df[1], 1)
    ll, r1 = limited(lf, 15)
    dl, r2 = limited(df, 15)
    hlit = 286
    while hlit > 257 and ll[hlit - 1] == 0:
        hlit -= 1
    hdist = 30
    while hdist > 1 and dl[hdist - 1] == 0:
        hdist -= 1
    cl_syms = rle(ll[:hlit] + dl[:hdist])
    cf = [0] * 19
    for s, _ in cl_syms:
        cf[s] += 1
    cl, r3 = limited(cf, 7)
    hclen = 19
    while hclen > 4 and cl[CL_ORDER[hclen - 1]] == 0:
        hclen -= 1
    dyn = 3 + 14 + 3 * hclen + data_bits(lits, dists, ll, dl)
    for s, _ in cl_syms:
        dyn += cl[s] + CL_EXTRA.get(s, 0)
    fl, fd = fixed_lengths()
    fixed = 3 + data_bits(lits, dists, fl, fd)
    stored = 8 + 32 + 8 * len(d) if len(d) <= 65535 else 1 << 40
    kind, best = "stored", stored
    if fixed <= best:
        kind, best = "fixed", fixed
    if dyn < best:
        kind, best = "dynamic", dyn
    w = Bits()
    if kind == "stored":
        w.put(1, 1)
        w.put(0, 2)
        w.flush()
        n = len(d)
        w.out += bytes([n & 0xFF, n >> 8, (~n) & 0xFF, ((~n) >> 8) & 0xFF]) + bytes(d)
    else:
        w.put(1, 1)
        if kind == "fixed":
            w.put(1, 2)
            ll, dl = fl, fd
        else:
            w.put(2, 2)
            w.put(hlit - 257, 5)
            w.put(hdist - 1, 5)
            w.put(hclen - 4, 4)
            for k in range(hclen):
                w.put(cl[CL_ORDER[k]], 3)
            cc = canonical(cl)
            for s, e in cl_syms:
                w.huff(cc[s], cl[s])
                if s >= 16:
                    w.put(e, CL_EXTRA[s])
        lc_, dc_ = canonical(ll), canonical(dl)
        for v, dd in zip(lits, dists):
            if dd == 0:
                w.huff(lc_[v], ll[v])
            else:
                lc, dc = length_code(v), dist_code(dd)
                w.huff(lc_[257 + lc], ll[257 + lc])
                w.put(v - LBASE[lc], LEXTRA[lc])
                w.huff(dc_[dc], dl[dc])
                w.put(dd - DBASE[dc], DEXTRA[dc])
        w.huff(lc_[256], ll[256])
        w.flush()
    info = f"{kind} (bits: stored {stored if len(d) <= 65535 else 'n/a'}, fixed {fixed}, dynamic {dyn}; " \
           f"{len(lits)} tokens, {sum(1 for x in dists if x)} matches, rescales {r1 + r2 + r3})"
    return bytes(w.out), info


# ---------------------------------------------------------------- inflate

class Reader:
    def __init__(self, data, pos):
        self.d, self.pos, self.buf, self.cnt = data, pos, 0, 0

    def bits(self, n):
        v = self.buf
        while self.cnt < n:
            if self.pos >= len(self.d):
                raise Bad("unexpected end of stream")
            v |= self.d[self.pos] << self.cnt
            self.pos += 1
            self.cnt += 8
        self.buf = v >> n
        self.cnt -= n
        return v & ((1 << n) - 1)


class Huff:
    def __init__(self, lengths):
        self.count = [0] * 16
        for x in lengths:
            self.count[x] += 1
        left = 1
        self.left = 0
        for b in range(1, 16):
            left = (left << 1) - self.count[b]
            if left < 0:
                self.left = -1
                break
        else:
            self.left = left
        offs = [0] * 16
        for b in range(1, 15):
            offs[b + 1] = offs[b] + self.count[b]
        self.symbol = [0] * len(lengths)
        for s, x in enumerate(lengths):
            if x:
                self.symbol[offs[x]] = s
                offs[x] += 1


def decode(r, h):
    code = first = index = 0
    for b in range(1, 16):
        code |= r.bits(1)
        count = h.count[b]
        if code - count < first:
            return h.symbol[index + (code - first)]
        index += count
        first += count
        first <<= 1
        code <<= 1
    raise Bad("ran out of codes")


def codes(r, out, lc, dc):
    while True:
        s = decode(r, lc)
        if s < 256:
            out.append(s)
        elif s == 256:
            return
        else:
            s -= 257
            if s >= 29:
                raise Bad(f"invalid length symbol {s + 257}")
            n = LBASE[s] + r.bits(LEXTRA[s])
            ds = decode(r, dc)
            if ds >= 30:
                raise Bad(f"invalid distance symbol {ds}")
            dist = DBASE[ds] + r.bits(DEXTRA[ds])
            if dist > len(out):
                raise Bad(f"distance {dist} too far back (have {len(out)})")
            for _ in range(n):
                out.append(out[len(out) - dist])


def inflate(data, pos):
    r = Reader(data, pos)
    out = bytearray()
    kinds = []
    while True:
        last = r.bits(1)
        t = r.bits(2)
        if t == 0:
            kinds.append("stored")
            r.buf, r.cnt = 0, 0
            if r.pos + 4 > len(data):
                raise Bad("unexpected end of stream")
            n = data[r.pos] | data[r.pos + 1] << 8
            nn = data[r.pos + 2] | data[r.pos + 3] << 8
            if n != (~nn) & 0xFFFF:
                raise Bad("stored length does not match its complement")
            r.pos += 4
            if r.pos + n > len(data):
                raise Bad("unexpected end of stream")
            out += data[r.pos:r.pos + n]
            r.pos += n
        elif t == 1:
            kinds.append("fixed")
            fl, fd = fixed_lengths()
            codes(r, out, Huff(fl), Huff(fd))
        elif t == 2:
            kinds.append("dynamic")
            nlen = r.bits(5) + 257
            ndist = r.bits(5) + 1
            ncode = r.bits(4) + 4
            if nlen > 286 or ndist > 30:
                raise Bad(f"bad code counts {nlen}/{ndist}")
            cl = [0] * 19
            for k in range(ncode):
                cl[CL_ORDER[k]] = r.bits(3)
            ch = Huff(cl)
            if ch.left != 0:
                raise Bad("incomplete code-length code")
            lens = []
            while len(lens) < nlen + ndist:
                s = decode(r, ch)
                if s < 16:
                    lens.append(s)
                else:
                    if s == 16:
                        if not lens:
                            raise Bad("repeat with no previous length")
                        v, rep = lens[-1], 3 + r.bits(2)
                    elif s == 17:
                        v, rep = 0, 3 + r.bits(3)
                    else:
                        v, rep = 0, 11 + r.bits(7)
                    if len(lens) + rep > nlen + ndist:
                        raise Bad("too many code lengths")
                    lens += [v] * rep
            if lens[256] == 0:
                raise Bad("no end-of-block code")
            lh = Huff(lens[:nlen])
            if lh.left < 0 or (lh.left > 0 and nlen - lh.count[0] != 1):
                raise Bad("bad literal/length code")
            dh = Huff(lens[nlen:])
            if dh.left < 0 or (dh.left > 0 and ndist - dh.count[0] != 1):
                raise Bad("bad distance code")
            codes(r, out, lh, dh)
        else:
            raise Bad("invalid block type 3")
        if last:
            return bytes(out), r.pos, kinds


# ---------------------------------------------------------------- gzip

def le32(v):
    return bytes([(v >> s) & 0xFF for s in (0, 8, 16, 24)])


def gzip(d):
    body, info = deflate(d)
    return bytes([0x1F, 0x8B, 8, 0]) + le32(MTIME) + bytes([0, 3]) + body + le32(crc32(d)) + le32(len(d)), info


def gunzip(g):
    if len(g) < 18 or g[0] != 0x1F or g[1] != 0x8B:
        raise Bad("not a gzip file")
    if g[2] != 8:
        raise Bad(f"unsupported method {g[2]}")
    flags = g[3]
    pos = 10
    if flags & 4:
        pos += 2 + (g[pos] | g[pos + 1] << 8)
    for bit in (8, 16):
        if flags & bit:
            while pos < len(g) and g[pos] != 0:
                pos += 1
            pos += 1
    if flags & 2:
        pos += 2
    out, end, kinds = inflate(g, pos)
    if end + 8 > len(g):
        raise Bad("missing trailer")
    stored = int.from_bytes(g[end:end + 4], "little")
    size = int.from_bytes(g[end + 4:end + 8], "little")
    if stored != crc32(out):
        raise Bad(f"crc mismatch: trailer {stored:08x}, data {crc32(out):08x}")
    if size != len(out) & 0xFFFFFFFF:
        raise Bad(f"size mismatch: trailer {size}, data {len(out)}")
    if end + 8 != len(g):
        raise Bad(f"{len(g) - end - 8} bytes after the trailer")
    return out, kinds


# ---------------------------------------------------------------- tar

def field(h, at, width, text):
    b = text.encode()
    h[at:at + len(b)] = b


def header(name, size, mode, kind):
    h = bytearray(512)
    if len(name) > 100:
        p = name.find("/")
        while p >= 0 and len(name) - p - 1 > 100:
            p = name.find("/", p + 1)
        if p < 0 or p > 155:
            raise Bad(f"name too long for ustar: {name}")
        field(h, 345, 155, name[:p])
        name = name[p + 1:]
    field(h, 0, 100, name)
    field(h, 100, 8, octal(mode, 7))
    field(h, 108, 8, octal(1000, 7))
    field(h, 116, 8, octal(1000, 7))
    field(h, 124, 12, octal(size, 11))
    field(h, 136, 12, octal(MTIME, 11))
    field(h, 148, 8, "        ")
    h[156] = ord(kind)
    field(h, 257, 6, "ustar")
    field(h, 263, 2, "00")
    field(h, 265, 32, "kara")
    field(h, 297, 32, "kara")
    field(h, 329, 8, octal(0, 7))
    field(h, 337, 8, octal(0, 7))
    field(h, 148, 8, octal(sum(h), 6))
    h[154] = 0
    h[155] = ord(" ")
    return h


def tar(entries):
    out = bytearray()
    for name, data in entries:
        if data is None:
            out += header(name, 0, 0o755, "5")
        else:
            out += header(name, len(data), 0o644, "0") + data
            out += bytes((512 - len(data) % 512) % 512)
    out += bytes(1024)
    out += bytes((10240 - len(out) % 10240) % 10240)
    return bytes(out)


def cstr(b):
    end = b.find(0)
    return (b if end < 0 else b[:end]).decode()


def parse_octal(b, what, at):
    v = 0
    seen = False
    for c in b:
        if c in (0, 32):
            if seen:
                break
            continue
        if c < 48 or c > 55:
            raise Bad(f"bad octal {what} in header at {at}")
        v = v * 8 + c - 48
        seen = True
    return v


def untar(t):
    out = []
    at = 0
    while True:
        if at + 512 > len(t):
            raise Bad(f"archive ends inside a header at {at}")
        h = t[at:at + 512]
        if not any(h):
            if at + 1024 > len(t) or any(t[at + 512:at + 1024]):
                raise Bad(f"lone zero block at {at}")
            return out
        want = parse_octal(h[148:156], "checksum", at)
        got = sum(h[:148]) + 8 * 32 + sum(h[156:])
        if want != got:
            raise Bad(f"header checksum at {at}: stored {want}, computed {got}")
        if h[257:262] != b"ustar":
            raise Bad(f"not a ustar header at {at}")
        name = cstr(h[0:100])
        prefix = cstr(h[345:500])
        if prefix:
            name = prefix + "/" + name
        size = parse_octal(h[124:136], "size", at)
        if at + 512 + size > len(t):
            raise Bad(f"{name}: data runs past the end")
        out.append((name, parse_octal(h[100:108], "mode", at), chr(h[156]), cstr(h[265:297]),
                    cstr(h[297:329]), parse_octal(h[136:148], "mtime", at), t[at + 512:at + 512 + size]))
        at += 512 + (size + 511) // 512 * 512


def perms(mode, kind):
    s = "d" if kind == "5" else "-"
    for k in range(9):
        s += "rwx"[k % 3] if mode & (1 << (8 - k)) else "-"
    return s


def when(t):
    return f"{dt.datetime.fromtimestamp(t, dt.timezone.utc):%Y-%m-%d %H:%M}"


# ---------------------------------------------------------------- main

REFERENCE = bytes.fromhex(
    "1f8b0800000000000203ed8f3172c3300c047bbde21ee0d1075cbbb2d365dcc3242822910019a4a438af0f95cca4c8"
    "133ce900cce16eef4c4e0836cd32724135285559b99d22a366b765c8b85cae2f079046482db04dd94b961953138d104d"
    "6dc7cddc6d2b7d775ad91f488b862aa6881c46f266beff724a1c6a39627659a9f2afac20d3ba27f2f463e81cfbee3533"
    "ee8b8477dc9ab722d907de96696e102d645763a4cf07a20d8d6f20d16fca3f53df9dff5b3e4dcb68ca7df7058b360480"
    "b8020000")

LONG_DIR = "site/a_rather_long_directory_name_that_pushes_the_path_past_the_ustar_limit"


def site_files():
    page = "<!doctype html>\n<html><head><title>Kara</title></head>\n<body>\n"
    for k in range(12):
        page += f'  <p class="row">Row {k}: the same markup, over and over, compresses well.</p>\n'
    page += "</body></html>\n"
    seed = 12345
    blob = bytearray()
    for _ in range(2000):
        seed = (seed * 1103515245 + 12345) % 2147483648
        blob.append((seed >> 16) & 0xFF)
    return [
        ("site/index.html", page.encode()),
        ("site/notes.txt", "Ship it.\nThen measure it.\n".encode()),
        ("site/empty.txt", b""),
        ("site/data.bin", bytes(blob)),
        (LONG_DIR + "/of_one_hundred_bytes.txt", "deep\n".encode()),
    ]


def write(path, data):
    with open(path, "wb") as f:
        f.write(data)


def read(path):
    with open(path, "rb") as f:
        return f.read()


def main():
    print(f"crc32(\"123456789\") = {crc32(b'123456789'):08x}")
    print(f"length codes: 3 -> {length_code(3) + 257}, 10 -> {length_code(10) + 257}, 11 -> {length_code(11) + 257}, "
          f"257 -> {length_code(257) + 257}, 258 -> {length_code(258) + 257}")
    print(f"distance codes: 1 -> {dist_code(1)}, 5 -> {dist_code(5)}, 24577 -> {dist_code(24577)}, 32768 -> {dist_code(32768)}")

    files = site_files()
    os.makedirs(LONG_DIR, exist_ok=True)
    for name, data in files:
        write(name, data)
    print(f"wrote {len(files)} files under site/")

    entries = [("site/", None)]
    for name, _ in files:
        if name.startswith(LONG_DIR + "/") and (LONG_DIR + "/", None) not in entries:
            entries.append((LONG_DIR + "/", None))
        entries.append((name, read(name)))
    archive = tar(entries)
    print(f"tar: {len(entries)} entries, {len(archive)} bytes")

    for name, data in [("empty.txt", b""), ("notes.txt", files[1][1]), ("data.bin", files[3][1]),
                       ("index.html", files[0][1]), ("site.tar", archive)]:
        body, info = deflate(data)
        print(f"deflate {name}: {len(data)} -> {len(body)} bytes, {info}")

    gz, _ = gzip(archive)
    write("site.tar.gz", gz)
    print(f"wrote site.tar.gz: {len(gz)} bytes, crc32 {crc32(archive):08x}")

    back, kinds = gunzip(read("site.tar.gz"))
    print(f"gunzip: {len(back)} bytes, blocks {kinds}, identical {'true' if back == archive else 'false'}")
    listing = untar(back)
    for name, mode, kind, u, g, mt, data in listing:
        print(f"  {perms(mode, kind)} {u}/{g} {len(data):>6} {when(mt)} {name}")
    os.makedirs("out", exist_ok=True)
    same = 0
    for name, mode, kind, u, g, mt, data in listing:
        path = "out/" + name
        if kind == "5":
            os.makedirs(path, exist_ok=True)
        else:
            write(path, data)
            if read(path) == read(name):
                same += 1
    print(f"extracted into out/: {same} of {len(files)} files identical to the originals")

    ref, kinds = gunzip(REFERENCE)
    text = ref.decode()
    print(f"reference stream from zlib -9: {len(REFERENCE)} -> {len(ref)} bytes, blocks {kinds}, "
          f"crc {crc32(ref):08x}, {text.count(chr(10))} lines, last [{text.splitlines()[-1]}]")
    again, _ = gzip(ref)
    print(f"our gzip of the same text: {len(again)} bytes")

    print("== damage")
    cases = []
    g = bytearray(gz)
    g[-8] ^= 0xFF
    cases.append(("flipped trailer crc", bytes(g)))
    cases.append(("truncated by 20 bytes", gz[:-20]))
    g = bytearray(gz)
    g[40] ^= 0x10
    cases.append(("flipped bit in byte 40", bytes(g)))
    cases.append(("extra byte after trailer", gz + b"\0"))
    cases.append(("not gzip", archive[:64]))
    for label, data in cases:
        try:
            gunzip(data)
            print(f"  {label}: accepted")
        except Bad as e:
            print(f"  {label}: {e}")
    t = bytearray(archive)
    t[600] ^= 1
    try:
        untar(bytes(t))
        print("  tar with a flipped header byte: accepted")
    except Bad as e:
        print(f"  tar with a flipped header byte: {e}")
    try:
        untar(archive[:1500])
        print("  truncated tar: accepted")
    except Bad as e:
        print(f"  truncated tar: {e}")


main()
