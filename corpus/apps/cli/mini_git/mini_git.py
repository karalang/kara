"""Mirror of mini_git.kara: a small git writing a real .git directory.

Uses hashlib and zlib where the Kara program has its own SHA-1 and
inflate, and writes the same stored-block zlib streams (level 0).
"""
import hashlib
import os
import subprocess
import zlib

ROOT = "proj"
AUTHOR = "Ada Lovelace <ada@example.com> 1700000000 +0000"
REAL = [
    ("0769cd4d9046198627f6e63d1ad02a7c34456c86",
     "780105c1cb0d8020140440cf54b10578f3603d7c1625597806d044ab7726c802f66d39291972b78a4e2f1c65bac1682d41a57145b47a758ec184f0e25309f013f32412b3bf35213e94fb017f551c72"),
    ("51c58a0ee0d53a01e061d94002e84926562b9c07", "78014bcac94f52306528c9ccabe4020019b103c3"),
]


class Fail(Exception):
    pass


def sha1(b):
    return hashlib.sha1(b).hexdigest()


def sh(script):
    r = subprocess.run(["sh", "-c", f"mkdir -p {ROOT} && cd {ROOT} && {script}"], capture_output=True, text=True)
    if r.returncode != 0:
        raise Fail(f"exit {r.returncode}: {r.stderr.strip()}")
    return r.stdout


def read_bytes(path):
    try:
        with open(f"{ROOT}/{path}", "rb") as f:
            return f.read()
    except FileNotFoundError:
        raise Fail(f"{path}: not found")


def write_bytes(path, b):
    with open(f"{ROOT}/{path}", "wb") as f:
        f.write(b)


def write_file(path, text):
    if "/" in path:
        sh(f"mkdir -p '{path[:path.rfind('/')]}'")
    write_bytes(path, text.encode())


def text_of(b):
    try:
        return b.decode("utf-8")
    except UnicodeDecodeError:
        return "<binary>"


def lines(s):
    return s.splitlines()


def work_files():
    out = []
    for l in lines(sh("find . -type f -not -path './.git/*'")):
        if l.startswith("./"):
            out.append(l[2:])
    return sorted(out)


def object_path(i):
    return f".git/objects/{i[:2]}/{i[2:]}"


def frame(kind, body):
    return f"{kind} {len(body)}".encode() + b"\0" + body


def zlib_store(raw):
    return zlib.compress(raw, 0)


def write_object(kind, body):
    raw = frame(kind, body)
    i = sha1(raw)
    sh(f"mkdir -p .git/objects/{i[:2]}")
    write_bytes(object_path(i), zlib_store(raw))
    return i


def parse_object(raw):
    nul = raw.find(b"\0")
    sp = raw.find(b" ")
    if sp < 0 or nul < 0:
        raise Fail("object: no header")
    try:
        n = int(raw[sp + 1:nul].decode())
    except ValueError:
        raise Fail("object: bad length")
    if nul + 1 + n != len(raw):
        raise Fail(f"object: length {n} but {len(raw) - nul - 1} bytes")
    return text_of(raw[:sp]), raw[nul + 1:]


def read_object(i):
    raw = zlib.decompress(read_bytes(object_path(i)))
    if sha1(raw) != i:
        raise Fail(f"object {i[:7]} does not hash to its name")
    return parse_object(raw)


def parse_tree(body):
    out = []
    at = 0
    while at < len(body):
        sp = body.index(b" ", at)
        nul = body.index(b"\0", sp)
        out.append((text_of(body[at:sp]), text_of(body[sp + 1:nul]), body[nul + 1:nul + 21].hex()))
        at = nul + 21
    return out


def write_tree(entries):
    subdirs = {}
    ordered = {}
    for path in sorted(entries):
        i = entries[path]
        if "/" in path:
            d, rest = path.split("/", 1)
            subdirs.setdefault(d, {})[rest] = i
        else:
            ordered[path] = ("100644", path, i)
    for d in sorted(subdirs):
        ordered[d + "/"] = ("40000", d, write_tree(subdirs[d]))
    body = b""
    for key in sorted(ordered):
        mode, name, i = ordered[key]
        body += f"{mode} {name}".encode() + b"\0" + bytes.fromhex(i)
    return write_object("tree", body)


def read_tree(i, prefix, out):
    kind, body = read_object(i)
    if kind != "tree":
        raise Fail(f"{i[:7]} is a {kind}, not a tree")
    for mode, name, eid in parse_tree(body):
        path = prefix + name
        if mode == "40000":
            read_tree(eid, path + "/", out)
        else:
            out[path] = eid


def parse_commit(body):
    text = text_of(body)
    split = text.find("\n\n")
    if split < 0:
        raise Fail("commit: no message")
    tree, parent = "", None
    for line in lines(text[:split]):
        if line.startswith("tree "):
            tree = line[5:]
        elif line.startswith("parent "):
            parent = line[7:]
    return tree, parent, text[split + 2:].strip()


class Repo:
    def __init__(self):
        self.index = {}


def init():
    sh("mkdir -p .git/objects .git/refs/heads")
    write_file(".git/HEAD", "ref: refs/heads/main\n")
    return Repo()


def save_index(repo):
    write_file(".git/kara-index", "".join(f"{repo.index[p]} {p}\n" for p in sorted(repo.index)))


def head():
    try:
        return text_of(read_bytes(".git/refs/heads/main")).strip()
    except Fail:
        return None


def head_files():
    files = {}
    c = head()
    if c is not None:
        tree, _, _ = parse_commit(read_object(c)[1])
        read_tree(tree, "", files)
    return files


def add(repo, path):
    repo.index[path] = write_object("blob", read_bytes(path))
    save_index(repo)


def rm(repo, path):
    if path not in repo.index:
        raise Fail(f"{path} is not tracked")
    del repo.index[path]
    sh(f"rm -f '{path}'")
    save_index(repo)


def commit(repo, message):
    tree = write_tree(repo.index)
    text = f"tree {tree}\n"
    p = head()
    if p is not None:
        text += f"parent {p}\n"
    text += f"author {AUTHOR}\ncommitter {AUTHOR}\n\n{message}\n"
    i = write_object("commit", text.encode())
    write_file(".git/refs/heads/main", f"{i}\n")
    return i


def status(repo):
    committed = head_files()
    out = []
    for path in sorted(repo.index):
        i = repo.index[path]
        if path in committed:
            if committed[path] != i:
                out.append(f"staged:    modified {path}")
        else:
            out.append(f"staged:    new file {path}")
    for path in sorted(committed):
        if path not in repo.index:
            out.append(f"staged:    deleted  {path}")
    work = work_files()
    for path in sorted(repo.index):
        if path not in work:
            out.append(f"unstaged:  deleted  {path}")
        elif sha1(frame("blob", read_bytes(path))) != repo.index[path]:
            out.append(f"unstaged:  modified {path}")
    for w in work:
        if w not in repo.index:
            out.append(f"untracked: {w}")
    if not out:
        print("  nothing to commit, working tree clean")
    for l in out:
        print(f"  {l}")


def diff(old, new):
    a, b = lines(old), lines(new)
    n, m = len(a), len(b)
    t = [[0] * (m + 1) for _ in range(n + 1)]
    for i in range(n - 1, -1, -1):
        for j in range(m - 1, -1, -1):
            if a[i] == b[j]:
                t[i][j] = t[i + 1][j + 1] + 1
            else:
                t[i][j] = max(t[i + 1][j], t[i][j + 1])
    ops = []
    x = y = 0
    while x < n or y < m:
        if x < n and y < m and a[x] == b[y]:
            ops.append(" " + a[x])
            x += 1
            y += 1
        elif y < m and (x == n or t[x][y + 1] >= t[x + 1][y]):
            ops.append("+" + b[y])
            y += 1
        else:
            ops.append("-" + a[x])
            x += 1
    for k, op in enumerate(ops):
        near = (k > 0 and not ops[k - 1].startswith(" ")) or \
               (k + 1 < len(ops) and not ops[k + 1].startswith(" ")) or not op.startswith(" ")
        if near:
            print(f"  {op}")


def checkout(repo, i):
    tree, _, _ = parse_commit(read_object(i)[1])
    target = {}
    read_tree(tree, "", target)
    for path in sorted(repo.index):
        if path not in target:
            sh(f"rm -f '{path}'")
    for path in sorted(target):
        write_file(path, text_of(read_object(target[path])[1]))
    repo.index = target
    save_index(repo)
    write_file(".git/refs/heads/main", f"{i}\n")


def log():
    at = head()
    while at is not None:
        _, parent, msg = parse_commit(read_object(at)[1])
        print(f"  commit {at}")
        print(f"      {msg}")
        at = parent


def fsck():
    paths = sorted(lines(sh("find .git/objects -type f")))
    bad = 0
    for p in paths:
        i = p[13:15] + p[16:]
        try:
            read_object(i)
        except Fail as e:
            print(f"  error: {e}")
            bad += 1
    print(f"  {len(paths)} objects, {bad} bad")
    return bad


def run():
    print("== init")
    repo = init()
    write_file("README.md", "# demo\n\nA tiny project.\nIt has three parts.\nSee docs/guide.txt.\n")
    write_file("b.txt", "scratch notes\n")
    write_file("src/main.c", "int main(void) { return helper(); }\n")
    write_file("src/util/str.c", "int helper(void) { return 0; }\n")
    write_file("docs/guide.txt", "Build with make.\n")
    write_file("src.txt", "a file that sorts between src and src/\n")
    for p in work_files():
        add(repo, p)
    first = commit(repo, "initial import")
    print(f"  [main {first[:7]}] initial import")
    print("== status")
    status(repo)

    print("== change things")
    write_file("README.md", "# demo\n\nA tiny project.\nIt has four parts now.\nSee docs/guide.txt.\nMIT licensed.\n")
    sh("rm b.txt")
    write_file("NEWS.txt", "0.2: second release\n")
    write_file("src/util/str.c", "int helper(void) { return 1; }\n")
    add(repo, "src/util/str.c")
    status(repo)

    print("== diff README.md")
    committed = head_files()
    old = text_of(read_object(committed["README.md"])[1])
    diff(old, text_of(read_bytes("README.md")))

    print("== second commit")
    add(repo, "README.md")
    add(repo, "NEWS.txt")
    rm(repo, "b.txt")
    second = commit(repo, "second release")
    print(f"  [main {second[:7]}] second release")
    status(repo)

    print("== log")
    log()

    print("== ls-tree -r HEAD")
    files = head_files()
    for path in sorted(files):
        print(f"  {files[path]} {path}")

    print("== checkout first commit")
    checkout(repo, first)
    print(f"  files: {' '.join(work_files())}")
    print(f"  b.txt: {text_of(read_bytes('b.txt')).strip()}")
    status(repo)
    print("== checkout second again")
    checkout(repo, second)
    print(f"  files: {' '.join(work_files())}")

    print("== objects from real git")
    for i, hx in REAL:
        raw = zlib.decompress(bytes.fromhex(hx))
        ok = "ok" if sha1(raw) == i else "MISMATCH"
        kind, body = parse_object(raw)
        print(f"  {i[:7]} {kind} {len(body)} bytes, hash {ok}: {lines(text_of(body))[0]}")

    print("== fsck")
    fsck()
    print("== fsck after an object is rewritten")
    victim = repo.index["docs/guide.txt"]
    write_bytes(object_path(victim), zlib_store(frame("blob", b"Build with cmake.\n")))
    bad = fsck()
    try:
        read_object(victim)
        print("  read: ok")
    except Fail as e:
        print(f"  read: {e}")
    print(f"  exit status would be {1 if bad > 0 else 0}")


def main():
    try:
        run()
    except Fail as e:
        print(f"fatal: {e}")


main()
