# Mirror of btree_kv.kara: a page-based B+tree key-value store with a
# write-ahead log, over real binary files.
import os

PAGE = 256
DB = "kv.db"
WAL = "kv.wal"
LEAF = 1
INNER = 2

CRC = []
for n in range(256):
    c = n
    for _ in range(8):
        c = (0xEDB88320 ^ (c >> 1)) if c & 1 else c >> 1
    CRC.append(c)

def crc32(b):
    c = 0xFFFFFFFF
    for x in b:
        c = CRC[(c ^ x) & 0xFF] ^ (c >> 8)
    return c ^ 0xFFFFFFFF

def u16(b, at):
    return b[at] | (b[at + 1] << 8)

def u32(b, at):
    return b[at] | (b[at + 1] << 8) | (b[at + 2] << 16) | (b[at + 3] << 24)

def put16(out, v):
    out += bytes([v & 0xFF, (v >> 8) & 0xFF])

def put32(out, v):
    out += bytes([v & 0xFF, (v >> 8) & 0xFF, (v >> 16) & 0xFF, (v >> 24) & 0xFF])

class Node:
    def __init__(self, kind):
        self.kind = kind
        self.keys = []
        self.vals = []      # leaves
        self.children = []  # inner nodes
        self.next = 0       # leaves: next leaf page, 0 = none

    def size(self):
        if self.kind == LEAF:
            return 7 + sum(2 + len(k) + len(v) for k, v in zip(self.keys, self.vals))
        return 7 + sum(1 + len(k) + 4 for k in self.keys)

def bisect_right(keys, k):
    i = 0
    while i < len(keys) and keys[i] <= k:
        i += 1
    return i

def bisect_left(keys, k):
    i = 0
    while i < len(keys) and keys[i] < k:
        i += 1
    return i

class Tree:
    def __init__(self):
        self.pages = [None]  # page 0 is the header
        root = Node(LEAF)
        self.pages.append(root)
        self.root = 1
        self.count = 0
        self.commits = 0
        self.splits = 0

    def alloc(self, node):
        self.pages.append(node)
        return len(self.pages) - 1

    def get(self, key):
        n = self.pages[self.root]
        while n.kind == INNER:
            n = self.pages[n.children[bisect_right(n.keys, key)]]
        i = bisect_left(n.keys, key)
        if i < len(n.keys) and n.keys[i] == key:
            return n.vals[i]
        return None

    def insert(self, pid, key, val):
        n = self.pages[pid]
        if n.kind == LEAF:
            i = bisect_left(n.keys, key)
            if i < len(n.keys) and n.keys[i] == key:
                n.vals[i] = val
            else:
                n.keys.insert(i, key)
                n.vals.insert(i, val)
                self.count += 1
            if n.size() <= PAGE:
                return None
            mid = len(n.keys) // 2
            right = Node(LEAF)
            right.keys = n.keys[mid:]
            right.vals = n.vals[mid:]
            n.keys = n.keys[:mid]
            n.vals = n.vals[:mid]
            rid = self.alloc(right)
            right.next = n.next
            n.next = rid
            self.splits += 1
            return (right.keys[0], rid)
        i = bisect_right(n.keys, key)
        up = self.insert(n.children[i], key, val)
        if up is None:
            return None
        sep, rid = up
        n.keys.insert(i, sep)
        n.children.insert(i + 1, rid)
        if n.size() <= PAGE:
            return None
        mid = len(n.keys) // 2
        promote = n.keys[mid]
        right = Node(INNER)
        right.keys = n.keys[mid + 1:]
        right.children = n.children[mid + 1:]
        n.keys = n.keys[:mid]
        n.children = n.children[:mid + 1]
        self.splits += 1
        return (promote, self.alloc(right))

    def put(self, key, val):
        up = self.insert(self.root, key, val)
        if up is not None:
            sep, rid = up
            r = Node(INNER)
            r.keys = [sep]
            r.children = [self.root, rid]
            self.root = self.alloc(r)

    def delete(self, key):
        n = self.pages[self.root]
        while n.kind == INNER:
            n = self.pages[n.children[bisect_right(n.keys, key)]]
        i = bisect_left(n.keys, key)
        if i < len(n.keys) and n.keys[i] == key:
            del n.keys[i]
            del n.vals[i]
            self.count -= 1
            return True
        return False

    def scan(self, lo, hi):
        n = self.pages[self.root]
        while n.kind == INNER:
            n = self.pages[n.children[bisect_right(n.keys, lo)]]
        out = []
        while True:
            for k, v in zip(n.keys, n.vals):
                if k >= hi:
                    return out
                if k >= lo:
                    out.append((k, v))
            if n.next == 0:
                return out
            n = self.pages[n.next]

    def serialize(self):
        out = bytearray(b"KBT1")
        put32(out, len(self.pages))
        put32(out, self.root)
        put32(out, self.count)
        put32(out, self.commits)
        out += bytes(PAGE - len(out))
        for pid in range(1, len(self.pages)):
            n = self.pages[pid]
            pg = bytearray([n.kind])
            put16(pg, len(n.keys))
            if n.kind == LEAF:
                put32(pg, n.next)
                for k, v in zip(n.keys, n.vals):
                    pg += bytes([len(k), len(v)]) + k.encode() + v.encode()
            else:
                put32(pg, n.children[0])
                for k, c in zip(n.keys, n.children[1:]):
                    pg += bytes([len(k)]) + k.encode()
                    put32(pg, c)
            pg += bytes(PAGE - len(pg))
            out += pg
        return bytes(out)

def load(data):
    if len(data) < PAGE or data[:4] != b"KBT1":
        raise ValueError("bad magic")
    npages = u32(data, 4)
    if len(data) != npages * PAGE:
        raise ValueError(f"truncated: {len(data)} bytes for {npages} pages")
    t = Tree()
    t.pages = [None]
    t.root = u32(data, 8)
    t.count = u32(data, 12)
    t.commits = u32(data, 16)
    for pid in range(1, npages):
        b = data[pid * PAGE:(pid + 1) * PAGE]
        kind = b[0]
        n = Node(kind)
        cnt = u16(b, 1)
        at = 7
        if kind == LEAF:
            n.next = u32(b, 3)
            for _ in range(cnt):
                kl, vl = b[at], b[at + 1]
                n.keys.append(b[at + 2:at + 2 + kl].decode())
                n.vals.append(b[at + 2 + kl:at + 2 + kl + vl].decode())
                at += 2 + kl + vl
        elif kind == INNER:
            n.children.append(u32(b, 3))
            for _ in range(cnt):
                kl = b[at]
                n.keys.append(b[at + 1:at + 1 + kl].decode())
                n.children.append(u32(b, at + 1 + kl))
                at += 1 + kl + 4
        else:
            raise ValueError(f"page {pid}: bad type {kind}")
        t.pages.append(n)
    return t

# ------------------------------------------------------------ files

def read_file(path):
    with open(path, "rb") as f:
        return f.read()

def write_file(path, data):
    with open(path, "wb") as f:
        f.write(data)
        f.flush()
        os.fsync(f.fileno())

def wal_record(op, key, val):
    rec = bytearray([op, len(key), len(val)]) + key.encode() + val.encode()
    put32(rec, crc32(rec))
    return bytes(rec)

class Store:
    def __init__(self):
        self.tree = None
        self.logged = 0

    def open(self):
        if os.path.exists(DB):
            self.tree = load(read_file(DB))
            print(f"  opened {DB}: {len(self.tree.pages)} pages, {self.tree.count} keys, commit {self.tree.commits}")
        else:
            self.tree = Tree()
            print(f"  created a new store")
        if os.path.exists(WAL):
            data = read_file(WAL)
            at = 0
            n = 0
            while at < len(data):
                if at + 3 > len(data):
                    break
                kl, vl = data[at + 1], data[at + 2]
                end = at + 3 + kl + vl
                if end + 4 > len(data):
                    break
                if crc32(data[at:end]) != u32(data, end):
                    print(f"  wal: bad checksum at {at}")
                    break
                key = data[at + 3:at + 3 + kl].decode()
                val = data[at + 3 + kl:end].decode()
                self.apply(data[at], key, val)
                n += 1
                at = end + 4
            print(f"  wal: replayed {n} records, discarded {len(data) - at} trailing bytes")

    def apply(self, op, key, val):
        if op == 1:
            self.tree.put(key, val)
        else:
            self.tree.delete(key)

    def log(self, op, key, val):
        with open(WAL, "ab") as f:
            f.write(wal_record(op, key, val))
            f.flush()
            os.fsync(f.fileno())
        self.apply(op, key, val)
        self.logged += 1

    def commit(self):
        self.tree.commits += 1
        data = self.tree.serialize()
        write_file(DB + ".tmp", data)
        os.rename(DB + ".tmp", DB)
        write_file(WAL, b"")
        print(f"  commit {self.tree.commits}: {len(self.tree.pages)} pages, {self.tree.count} keys, {self.tree.splits} splits this session, crc {crc32(data):08x}")

# ------------------------------------------------------------ checks

def check(t):
    leaves = []
    inner = 0
    depths = set()
    errors = []
    def walk(pid, lo, hi, depth):
        nonlocal inner
        n = t.pages[pid]
        for a, b in zip(n.keys, n.keys[1:]):
            if a >= b:
                errors.append(f"page {pid}: keys out of order")
        for k in n.keys:
            if (lo is not None and k < lo) or (hi is not None and k >= hi):
                errors.append(f"page {pid}: key {k} outside its bounds")
        if n.kind == LEAF:
            depths.add(depth)
            leaves.append(pid)
            return
        inner += 1
        bounds = [lo] + n.keys + [hi]
        for i, c in enumerate(n.children):
            walk(c, bounds[i], bounds[i + 1], depth + 1)
    walk(t.root, None, None, 1)
    chain = []
    pid = leaves[0]
    while pid != 0:
        chain.append(pid)
        pid = t.pages[pid].next
    if chain != leaves:
        errors.append("leaf chain does not match the tree order")
    if len(depths) != 1:
        errors.append(f"leaves at depths {sorted(depths)}")
    empty = sum(1 for p in leaves if not t.pages[p].keys)
    total = sum(len(t.pages[p].keys) for p in leaves)
    if total != t.count:
        errors.append(f"count {t.count} but {total} keys in leaves")
    return errors, max(depths), len(leaves), inner, empty

# ------------------------------------------------------------ workload

class Rng:
    def __init__(self, seed):
        self.s = seed
    def next(self):
        self.s = (self.s * 1103515245 + 12345) % 2147483648
        return self.s >> 4

def op_of(rng, i):
    r = rng.next() % 100
    key = f"user:{rng.next() % 400:04d}"
    if r < 70:
        return (1, key, f"v{i}-" + "abcdefghijklmnopqrstuvwxyz"[:rng.next() % 24])
    if r < 90:
        return (2, key, "")
    return (3, key, "")

def run_ops(store, model, rng, start, count, commit_every, show_gets):
    gets = 0
    hits = 0
    for i in range(start, start + count):
        op, key, val = op_of(rng, i)
        if op == 3:
            got = store.tree.get(key)
            want = model.get(key)
            gets += 1
            if got is not None:
                hits += 1
            if got != want:
                print(f"  get {key}: got {got}, model has {want}")
            if gets <= show_gets:
                print(f"  get {key} -> {got if got is not None else '(missing)'}")
        else:
            store.log(op, key, val)
            if op == 1:
                model[key] = val
            else:
                model.pop(key, None)
        if commit_every and (i - start + 1) % commit_every == 0:
            store.commit()
    return gets, hits

def main():
    for p in (DB, WAL, DB + ".tmp"):
        if os.path.exists(p):
            os.remove(p)
    rng = Rng(2026)
    model = {}
    print("== phase 1: 600 operations, a commit every 100")
    s = Store()
    s.open()
    gets, hits = run_ops(s, model, rng, 0, 600, 100, 4)
    print(f"  {gets} gets, {hits} hits")

    print("== phase 2: 12 more operations logged, then a crash before the commit")
    lost = []
    for i in range(600, 612):
        op, key, val = op_of(rng, i)
        if op == 3:
            continue
        s.log(op, key, val)
        lost.append((op, key, val))
    wal = read_file(WAL)
    print(f"  wal holds {len(lost)} records, {len(wal)} bytes; tearing the last 3 bytes")
    write_file(WAL, wal[:-3])
    for op, key, val in lost[:-1]:
        if op == 1:
            model[key] = val
        else:
            model.pop(key, None)
    print(f"  lost on purpose: {'put' if lost[-1][0] == 1 else 'del'} {lost[-1][1]}")

    print("== phase 3: reopen and recover")
    s = Store()
    s.open()
    s.commit()

    print("== phase 4: verify")
    bad = 0
    for k in range(400):
        key = f"user:{k:04d}"
        if s.tree.get(key) != model.get(key):
            bad += 1
            print(f"  mismatch {key}: {s.tree.get(key)} vs {model.get(key)}")
    print(f"  {400 - bad} of 400 keys match the model ({len(model)} present)")
    errors, height, leaves, inner, empty = check(s.tree)
    for e in errors:
        print(f"  invariant: {e}")
    print(f"  invariants {'ok' if not errors else 'BROKEN'}: height {height}, {leaves} leaves, {inner} inner, {empty} empty leaves")

    print("== phase 5: scan [user:0040, user:0052)")
    for k, v in s.tree.scan("user:0040", "user:0052"):
        print(f"  {k} = {v}")

    print("== phase 6: damaged files")
    good = read_file(DB)
    bad_magic = b"XBT1" + good[4:]
    write_file("bad-magic.db", bad_magic)
    write_file("short.db", good[:len(good) - 100])
    for p in ("bad-magic.db", "short.db"):
        try:
            load(read_file(p))
            print(f"  {p}: loaded?")
        except ValueError as e:
            print(f"  {p}: {e}")
    t2 = load(good)
    print(f"  reloaded {DB}: {t2.count} keys, re-serialized identically: {t2.serialize() == good}")

main()
