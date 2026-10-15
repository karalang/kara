# Mirror of delta_sync.kara: an rsync-style sync of one directory onto
# another, with rolling checksums, MD5 block hashes and a binary delta
# format that is written to disk, read back and applied.
import os

BLOCK = 256
MOD = 65536

# ------------------------------------------------------------ MD5 (RFC 1321)

S = [7, 12, 17, 22] * 4 + [5, 9, 14, 20] * 4 + [4, 11, 16, 23] * 4 + [6, 10, 15, 21] * 4

def md5_k():
    # floor(abs(sin(i + 1)) * 2^32), computed with integers: the RFC's table
    return [
        0xd76aa478, 0xe8c7b756, 0x242070db, 0xc1bdceee, 0xf57c0faf, 0x4787c62a, 0xa8304613, 0xfd469501,
        0x698098d8, 0x8b44f7af, 0xffff5bb1, 0x895cd7be, 0x6b901122, 0xfd987193, 0xa679438e, 0x49b40821,
        0xf61e2562, 0xc040b340, 0x265e5a51, 0xe9b6c7aa, 0xd62f105d, 0x02441453, 0xd8a1e681, 0xe7d3fbc8,
        0x21e1cde6, 0xc33707d6, 0xf4d50d87, 0x455a14ed, 0xa9e3e905, 0xfcefa3f8, 0x676f02d9, 0x8d2a4c8a,
        0xfffa3942, 0x8771f681, 0x6d9d6122, 0xfde5380c, 0xa4beea44, 0x4bdecfa9, 0xf6bb4b60, 0xbebfbc70,
        0x289b7ec6, 0xeaa127fa, 0xd4ef3085, 0x04881d05, 0xd9d4d039, 0xe6db99e5, 0x1fa27cf8, 0xc4ac5665,
        0xf4292244, 0x432aff97, 0xab9423a7, 0xfc93a039, 0x655b59c3, 0x8f0ccc92, 0xffeff47d, 0x85845dd1,
        0x6fa87e4f, 0xfe2ce6e0, 0xa3014314, 0x4e0811a1, 0xf7537e82, 0xbd3af235, 0x2ad7d2bb, 0xeb86d391,
    ]

K = md5_k()
M32 = 0xFFFFFFFF

def rotl(x, n):
    return ((x << n) | (x >> (32 - n))) & M32

def md5(data, a0=0, a1=None):
    a1 = len(data) if a1 is None else a1
    msg = bytearray(data[a0:a1])
    bits = (len(msg) * 8) & 0xFFFFFFFFFFFFFFFF
    msg.append(0x80)
    while len(msg) % 64 != 56:
        msg.append(0)
    for i in range(8):
        msg.append((bits >> (8 * i)) & 0xFF)
    h = [0x67452301, 0xefcdab89, 0x98badcfe, 0x10325476]
    for off in range(0, len(msg), 64):
        m = [msg[off + 4 * j] | (msg[off + 4 * j + 1] << 8) | (msg[off + 4 * j + 2] << 16) | (msg[off + 4 * j + 3] << 24) for j in range(16)]
        a, b, c, d = h
        for i in range(64):
            if i < 16:
                f = (b & c) | (~b & d)
                g = i
            elif i < 32:
                f = (d & b) | (~d & c)
                g = (5 * i + 1) % 16
            elif i < 48:
                f = b ^ c ^ d
                g = (3 * i + 5) % 16
            else:
                f = c ^ (b | (~d & M32))
                g = (7 * i) % 16
            f = (f + a + K[i] + m[g]) & M32
            a, d, c = d, c, b
            b = (b + rotl(f, S[i])) & M32
        h = [(h[0] + a) & M32, (h[1] + b) & M32, (h[2] + c) & M32, (h[3] + d) & M32]
    return "".join(f"{(x >> (8 * k)) & 0xFF:02x}" for x in h for k in range(4))

# ------------------------------------------------------------ checksums

def weak(data, at, n):
    a = 0
    b = 0
    for i in range(n):
        a = (a + data[at + i]) % MOD
        b = (b + (n - i) * data[at + i]) % MOD
    return a, b

class Sig:
    def __init__(self):
        self.weak = []
        self.strong = []
        self.lens = []

def signature(data):
    s = Sig()
    at = 0
    while at < len(data):
        n = min(BLOCK, len(data) - at)
        a, b = weak(data, at, n)
        s.weak.append(b * MOD + a)
        s.strong.append(md5(data, at, at + n))
        s.lens.append(n)
        at += n
    return s

# ------------------------------------------------------------ deltas

OP_END = 0
OP_COPY = 1
OP_LIT = 2

def put32(out, v):
    for k in range(4):
        out.append((v >> (8 * k)) & 0xFF)

def get32(b, at):
    return b[at] | (b[at + 1] << 8) | (b[at + 2] << 16) | (b[at + 3] << 24)

class Stats:
    def __init__(self):
        self.copies = 0
        self.literal = 0
        self.probes = 0
        self.false_hits = 0

def flush(out, lit, st):
    if lit:
        out.append(OP_LIT)
        put32(out, len(lit))
        out.extend(lit)
        st.literal += len(lit)
        lit.clear()

def make_delta(src, sig, st):
    table = {}
    for k in range(len(sig.weak)):
        if sig.lens[k] == BLOCK:
            table.setdefault(sig.weak[k], []).append(k)
    out = bytearray(b"KDL1")
    lit = bytearray()
    n = len(src)
    i = 0
    a = b = 0
    if n >= BLOCK:
        a, b = weak(src, 0, BLOCK)
    while i + BLOCK <= n:
        match = -1
        w = b * MOD + a
        if w in table:
            st.probes += 1
            strong = md5(src, i, i + BLOCK)
            for k in table[w]:
                if sig.strong[k] == strong:
                    match = k
                    break
            if match < 0:
                st.false_hits += 1
        if match >= 0:
            flush(out, lit, st)
            out.append(OP_COPY)
            put32(out, match)
            st.copies += 1
            i += BLOCK
            if i + BLOCK <= n:
                a, b = weak(src, i, BLOCK)
            continue
        x = src[i]
        lit.append(x)
        if i + BLOCK < n:
            y = src[i + BLOCK]
            a = (a - x + y) % MOD
            b = (b - BLOCK * x + a) % MOD
        i += 1
    # the tail may match a short last block
    rest = n - i
    last = len(sig.lens) - 1
    if rest > 0 and last >= 0 and sig.lens[last] == rest and sig.strong[last] == md5(src, i, n):
        flush(out, lit, st)
        out.append(OP_COPY)
        put32(out, last)
        st.copies += 1
    else:
        lit.extend(src[i:])
    flush(out, lit, st)
    out.append(OP_END)
    out.extend(bytes.fromhex(md5(src)))
    return bytes(out)

class DeltaError(Exception):
    pass

def apply_delta(old, delta):
    if delta[:4] != b"KDL1":
        raise DeltaError("not a delta file")
    out = bytearray()
    at = 4
    while True:
        if at >= len(delta):
            raise DeltaError("truncated delta")
        op = delta[at]
        at += 1
        if op == OP_END:
            if at + 16 != len(delta):
                raise DeltaError("truncated delta")
            want = delta[at:at + 16].hex()
            got = md5(out)
            if got != want:
                raise DeltaError(f"checksum mismatch: {got} != {want}")
            return bytes(out)
        if at + 4 > len(delta):
            raise DeltaError("truncated delta")
        v = get32(delta, at)
        at += 4
        if op == OP_COPY:
            if v * BLOCK >= len(old):
                raise DeltaError(f"copy of block {v} past the old file")
            out.extend(old[v * BLOCK:min(len(old), v * BLOCK + BLOCK)])
        elif op == OP_LIT:
            if at + v > len(delta):
                raise DeltaError("truncated delta")
            out.extend(delta[at:at + v])
            at += v
        else:
            raise DeltaError(f"bad op {op}")

# ------------------------------------------------------------ the trees

class Rng:
    def __init__(self, seed):
        self.s = seed

    def next(self):
        self.s = (self.s * 1103515245 + 12345) % 2147483648
        return self.s >> 8

    def below(self, n):
        return self.next() % n

WORDS = ["the", "sync", "block", "rolling", "checksum", "file", "copy", "literal", "match",
         "window", "server", "client", "delta", "patch", "byte", "hash", "remote", "local"]

def prose(rng, paras):
    out = []
    for p in range(paras):
        words = [WORDS[rng.below(len(WORDS))] for _ in range(40 + rng.below(30))]
        out.append(f"{p + 1}. " + " ".join(words).capitalize() + ".\n")
    return "\n".join(out)

def binary(rng, n):
    return bytes(rng.below(256) for _ in range(n))

def write(path, data):
    with open(path, "wb") as f:
        f.write(data)

def read(path):
    with open(path, "rb") as f:
        return f.read()

def build_trees():
    rng = Rng(2026)
    for d in ("src", "dst", "deltas"):
        os.makedirs(d, exist_ok=True)
    notes = prose(rng, 12)
    data = binary(rng, 9000)
    config = "[server]\nport = 8080\nworkers = 4\n\n[sync]\nblock = 256\nverify = md5\n"
    log = "".join(f"2026-10-15T10:{i // 60:02}:{i % 60:02}Z sync ok files={rng.below(90)}\n" for i in range(140))
    paras = notes.split("\n\n")
    # the destination is an older copy: a paragraph added near the top,
    # one edited in the middle, bytes changed and cut in the data file,
    # the log a prefix of today's, and a file that has since been removed
    old_paras = list(paras)
    old_paras[6] = old_paras[6].replace("block", "BLOCK", 2)
    del old_paras[1]
    old_notes = "\n\n".join(old_paras)
    old_data = bytearray(data)
    for at in (700, 4100, 8800):
        old_data[at] ^= 0x5A
    del old_data[2000:2100]
    write("src/notes.txt", notes.encode())
    write("src/data.bin", data)
    write("src/config.ini", config.encode())
    write("src/log.txt", log.encode())
    write("src/README", b"Files in this directory are synced from the build host.\n")
    write("dst/notes.txt", old_notes.encode())
    write("dst/data.bin", bytes(old_data))
    write("dst/config.ini", config.encode())
    write("dst/log.txt", log[:len(log) * 3 // 5].encode())
    write("dst/stale.tmp", b"left over from a crashed run\n")

def sync(round_no):
    print(f"== round {round_no}")
    names = sorted(os.listdir("src"))
    have = set(os.listdir("dst"))
    sent = 0
    total = 0
    for name in names:
        src = read(f"src/{name}")
        total += len(src)
        if name in have:
            old = read(f"dst/{name}")
            if md5(old) == md5(src):
                print(f"  {name}: up to date ({len(src)} bytes)")
                continue
        else:
            old = b""
        st = Stats()
        sig = signature(old)
        delta = make_delta(src, sig, st)
        write(f"deltas/{name}.delta", delta)
        sent += len(delta)
        new = apply_delta(old, read(f"deltas/{name}.delta"))
        write(f"dst/{name}.part", new)
        os.rename(f"dst/{name}.part", f"dst/{name}")
        verb = "updated" if name in have else "created"
        print(f"  {name}: {verb}, {len(old)} -> {len(src)} bytes; {len(sig.weak)} blocks signed, "
              f"{st.copies} copied, {st.literal} literal bytes, {st.probes} probes "
              f"({st.false_hits} false), delta {len(delta)} bytes {md5(delta)}")
    for name in sorted(have):
        if name not in names:
            os.remove(f"dst/{name}")
            print(f"  {name}: deleted")
    print(f"  sent {sent} of {total} bytes")

def verify():
    a = sorted(os.listdir("src"))
    b = sorted(os.listdir("dst"))
    ok = a == b
    for name in a:
        h = md5(read(f"src/{name}"))
        same = name in b and md5(read(f"dst/{name}")) == h
        ok = ok and same
        print(f"  {h}  {name}{'' if same else '  DIFFERS'}")
    print(f"  trees {'match' if ok else 'DIFFER'}")

def damaged():
    print("== damaged deltas")
    old = read("dst/notes.txt")
    good = make_delta(read("src/notes.txt"), signature(old[:3000]), Stats())
    cases = [
        ("flipped literal byte", good[:20] + bytes([good[20] ^ 1]) + good[21:]),
        ("truncated", good[:len(good) - 9]),
        ("bad magic", b"KDL2" + good[4:]),
        ("copy past the end", b"KDL1" + bytes([OP_COPY]) + (999).to_bytes(4, "little") + good[4:]),
    ]
    for label, d in cases:
        write("deltas/bad.delta", d)
        try:
            apply_delta(old[:3000], read("deltas/bad.delta"))
            print(f"  {label}: APPLIED")
        except DeltaError as e:
            print(f"  {label}: refused, {e}")
    os.remove("deltas/bad.delta")

def main():
    build_trees()
    print(f"block size {BLOCK}; md5('') = {md5(b'')}")
    sync(1)
    print("== verify")
    verify()
    sync(2)
    damaged()

if __name__ == "__main__":
    main()
