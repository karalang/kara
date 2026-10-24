#!/usr/bin/env python3
"""Mirror of chunk_backup.kara: the same snapshots, chunking, store and reports.

Run as: python3 -I -B chunk_backup.py > expected.out
"""

WORDS = ['fn', 'let', 'mut', 'return', 'if', 'else', 'match', 'for', 'in', 'while',
         'struct', 'enum', 'impl', 'self', 'value', 'count', 'index', 'buffer', 'line', 'node',
         'parse', 'write', 'read', 'error', 'result', 'option', 'some', 'none', 'ok', 'err',
         'push', 'len', 'map', 'key', 'item', 'list', 'total', 'next', 'state', 'cache']

N_FILES = 60
MIN_CHUNK = 256
MAX_CHUNK = 4096
MASK = 0x3FF


class Lcg:
    def __init__(self, seed):
        self.state = seed

    def next(self, n):
        self.state = (self.state * 1103515245 + 12345) % 2147483648
        return (self.state >> 8) % n


def make_line(r):
    n = r.next(10) + 3
    indent = '    ' * r.next(3)
    return indent + ' '.join(WORDS[r.next(len(WORDS))] for _ in range(n))


def make_base():
    r = Lcg(2024)
    files = []
    for f in range(N_FILES):
        n = 120 + r.next(240)
        lines = [make_line(r) for _ in range(n)]
        files.append(['src/mod%02d.txt' % f, lines])
    return files


# Snapshot k edits its own copy of the base: most files stay, some get line
# edits, some grow, one file is new and, on even k, one is gone.
def make_snapshot(base, k):
    r = Lcg(k * 104729 + 7)
    files = []
    for name, lines in base:
        lines = list(lines)
        roll = r.next(100)
        if roll < 40:
            pass
        elif roll < 80:
            for _ in range(r.next(4) + 1):
                at = r.next(len(lines))
                kind = r.next(3)
                if kind == 0:
                    lines[at] = make_line(r)
                elif kind == 1:
                    lines.insert(at, make_line(r))
                else:
                    del lines[at]
        else:
            for _ in range(20):
                lines.append(make_line(r))
        files.append([name, lines])
    files.append(['src/new%d.txt' % k, [make_line(r) for _ in range(60 + r.next(60))]])
    if k % 2 == 0:
        del files[r.next(N_FILES)]
    files.sort(key=lambda f: f[0])
    return [(name, ('\n'.join(lines) + '\n').encode()) for name, lines in files]


def gear_table():
    r = Lcg(99)
    return [(r.next(65536) << 16) ^ r.next(65536) for _ in range(256)]


def fnv32(data):
    h = 2166136261
    for b in data:
        h = ((h ^ b) * 16777619) & 0xFFFFFFFF
    return h


def djb32(data):
    h = 5381
    for b in data:
        h = (h * 33 + b) & 0xFFFFFFFF
    return h


def cut_points(data, gear):
    cuts = []
    start = 0
    h = 0
    for i, b in enumerate(data):
        h = ((h << 1) + gear[b]) & 0xFFFFFFFF
        size = i + 1 - start
        if (size >= MIN_CHUNK and (h & MASK) == 0) or size >= MAX_CHUNK:
            cuts.append((start, size))
            start = i + 1
            h = 0
    if start < len(data):
        cuts.append((start, len(data) - start))
    return cuts


def chunk_snapshot(snap, gear):
    out = []
    for name, data in snap:
        refs = []
        for start, size in cut_points(data, gear):
            piece = data[start:start + size]
            refs.append((fnv32(piece), djb32(piece), size))
        out.append((name, refs))
    return out


class Store:
    def __init__(self):
        self.ids = {}
        self.data = []
        self.first = []
        self.refs = []


def add_snapshot(store, k, snap, chunks):
    for (name, data), (_, refs) in zip(snap, chunks):
        pos = 0
        for key in refs:
            size = key[2]
            if key not in store.ids:
                store.ids[key] = len(store.data)
                store.data.append(data[pos:pos + size])
                store.first.append(k)
                store.refs.append(0)
            store.refs[store.ids[key]] += 1
            pos += size


def snapshot_report(store, all_chunks):
    lines = ['snapshots:']
    for k, chunks in enumerate(all_chunks, 1):
        nbytes = sum(key[2] for _, refs in chunks for key in refs)
        nchunks = sum(len(refs) for _, refs in chunks)
        new = sum(1 for f in store.first if f == k)
        lines.append('  %d: %d files, %d bytes, %d chunks, %d new' % (k, len(chunks), nbytes, nchunks, new))
    return '\n'.join(lines)


def dedupe_report(store, all_chunks):
    logical = sum(key[2] for chunks in all_chunks for _, refs in chunks for key in refs)
    stored = sum(len(d) for d in store.data)
    ratio = logical * 100 // stored
    return 'dedupe: %d logical bytes, %d stored in %d chunks, ratio %d.%02dx' % (
        logical, stored, len(store.data), ratio // 100, ratio % 100)


def changes_report(a, b, ka, kb):
    old = dict(a)
    new = dict(b)
    added = sorted(n for n in new if n not in old)
    removed = sorted(n for n in old if n not in new)
    modified = 0
    same = 0
    changed_bytes = 0
    for name in sorted(new):
        if name not in old:
            continue
        if old[name] == new[name]:
            same += 1
            continue
        modified += 1
        seen = set(old[name])
        changed_bytes += sum(key[2] for key in new[name] if key not in seen)
    return 'changes %d->%d: %d same, %d modified (%d new bytes), added %s, removed %s' % (
        ka, kb, same, modified, changed_bytes, ','.join(added) or '-', ','.join(removed) or '-')


def size_histogram(store):
    buckets = {}
    for d in store.data:
        b = len(d) // 512
        buckets[b] = buckets.get(b, 0) + 1
    lines = ['chunk sizes:']
    for b in sorted(buckets):
        lines.append('  %d-%d: %d' % (b * 512, b * 512 + 511, buckets[b]))
    return '\n'.join(lines)


def top_shared(store, n):
    keys = sorted(store.ids.items(), key=lambda kv: kv[1])
    ranked = sorted(((store.refs[i], key) for key, i in keys), key=lambda rk: (-rk[0], rk[1]))
    lines = ['most shared chunks:']
    for refs, key in ranked[:n]:
        lines.append('  %08x %d bytes, %d refs' % (key[0], key[2], refs))
    return '\n'.join(lines)


def verify(store, k, snap, chunks):
    ok = 0
    for (name, data), (_, refs) in zip(snap, chunks):
        out = bytearray()
        for key in refs:
            out += store.data[store.ids[key]]
        if fnv32(out) == fnv32(data) and len(out) == len(data):
            ok += 1
    return 'snapshot %d: %d of %d files restore' % (k, ok, len(snap))


def main():
    base = make_base()
    gear = gear_table()
    snaps = [make_snapshot(base, k) for k in range(1, 5)]
    chunks = [chunk_snapshot(s, gear) for s in snaps]
    store = Store()
    for k in range(4):
        add_snapshot(store, k + 1, snaps[k], chunks[k])
    print(snapshot_report(store, chunks))
    print(dedupe_report(store, chunks))
    print(changes_report(chunks[0], chunks[1], 1, 2))
    print(changes_report(chunks[1], chunks[2], 2, 3))
    print(changes_report(chunks[2], chunks[3], 3, 4))
    print(size_histogram(store))
    print(top_shared(store, 3))
    for k in range(4):
        print(verify(store, k + 1, snaps[k], chunks[k]))


if __name__ == '__main__':
    main()
