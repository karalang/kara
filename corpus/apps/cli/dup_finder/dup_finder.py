# Mirror of source.kara: same tree, same FNV-1a 64 chunked scan, same output.
import csv, io, os, shutil, subprocess

CHUNK = 64

def pattern(n, seed):
    return bytes(((i * 31 + seed) % 251) for i in range(n))

def build_tree():
    shutil.rmtree("tree", ignore_errors=True)
    for d in ["tree/docs", "tree/backup/old", "tree/img", "tree/data"]:
        os.makedirs(d, exist_ok=True)
    texts = [
        ("tree/docs/readme.txt", "hello world\n"),
        ("tree/backup/readme.txt", "hello world\n"),
        ("tree/backup/old/readme.copy", "hello world\n"),
        ("tree/docs/notes.txt", "hello wordl\n"),
        ("tree/docs/my notes.txt", "hello wordl\n"),
        ("tree/docs/it's.txt", "unique text, nobody else has it\n"),
        ("tree/empty", ""),
        ("tree/docs/empty", ""),
    ]
    for p, t in texts:
        open(p, "w").write(t)
    logo = pattern(300, 7)
    logo2 = bytearray(logo); logo2[299] ^= 1
    big = pattern(5000, 3)
    blobs = [("tree/img/logo.bin", logo), ("tree/backup/logo.bin", logo),
             ("tree/img/logo2.bin", bytes(logo2)), ("tree/data/a.log", big),
             ("tree/data/b.log", big), ("tree/backup/a,b.log", big),
             ("tree/backup/old/a.log", big)]
    for p, b in blobs:
        open(p, "wb").write(b)
    return len(texts) + len(blobs)

def listing(root):
    out = []
    for d, _, fs in os.walk(root):
        for f in fs:
            out.append(os.path.join(d, f))
    # Kara sorts Strings bytewise
    return sorted(out, key=lambda s: s.encode())

def scan(p):
    h = 14695981039346656037
    size = 0
    with open(p, "rb") as f:
        while True:
            b = f.read(CHUNK)
            if not b:
                break
            for x in b:
                h = ((h ^ x) * 1099511628211) & 0xFFFFFFFFFFFFFFFF
            size += len(b)
    return size, h

def csv_field(s):
    if not ("," in s or '"' in s or "\n" in s):
        return s
    return '"' + s.replace('"', '""') + '"'

def main():
    print(f"wrote {build_tree()} files")
    paths = listing("tree")
    print(f"found {len(paths)} files")
    empties = 0; total = 0; groups = {}
    for p in paths:
        size, h = scan(p)
        total += size
        if size == 0:
            empties += 1; continue
        groups.setdefault(f"{size:08}-{h:016x}", []).append((p, size, h))
    print(f"scanned {total} bytes, {empties} empty files skipped")
    wasted = 0; ng = 0; removals = []
    for key in sorted(groups):
        g = groups[key]
        if len(g) < 2:
            continue
        keep = g[0][0]
        members = []
        for p, size, _ in g:
            if p == keep:
                continue
            if open(keep, "rb").read() == open(p, "rb").read():
                members.append(p); removals.append((keep, p, size))
            else:
                print(f"hash collision: {keep} vs {p}")
        if not members:
            continue
        ng += 1
        wasted += g[0][1] * len(members)
        print(f"group {ng}: {g[0][1]} bytes, hash {g[0][2]:016x}")
        print(f"  keep   {keep}")
        for m in members:
            print(f"  copy   {m}")
    print(f"{ng} duplicate groups, {len(removals)} copies, {wasted} bytes wasted")
    rows = ["kept,removed,bytes"]
    for keep, gone, size in removals:
        os.remove(gone)
        rows.append(f"{csv_field(keep)},{csv_field(gone)},{size}")
    print(f"removed {len(removals)} files")
    open("dups.csv", "w").write("\n".join(rows) + "\n")
    after = listing("tree")
    left = sum(scan(p)[0] for p in after)
    print(f"after: {len(after)} files, {left} bytes ({total - left} freed)")
    for p in after:
        print(f"  {p}")
    lines = open("dups.csv").read().splitlines()
    print(f"manifest: {len(lines) - 1} rows")
    for l in lines:
        print(f"  {l}")

main()
