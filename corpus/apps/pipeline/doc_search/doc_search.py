# Python mirror of doc_search.kara: the same tokenizer, per-document index,
# query parser, ranking, snippets, index file and incremental refresh.
# Documents are indexed on a thread pool of three workers that put their
# results on a queue; the merge sorts by name, as the Kara program does.
import queue
from concurrent.futures import ThreadPoolExecutor

INDEX = "search.idx"

STOPWORDS = "a an and are as at be by for from has in is it its of on or that the this to was were will with"

DOCS = [
    ("install.txt", "Installing Kestrel. Download the installer for your platform, then run it. The installer checks your disk space and asks for an install folder. Installing on Linux needs the tar archive and a shell."),
    ("config.txt", "Configuring Kestrel. Settings live in kestrel.toml. Each setting has a default; the config file overrides it, and environment variables override the file. Reloading the config does not restart running jobs."),
    ("jobs.txt", "Running jobs. A job is a command with inputs and outputs. Kestrel runs jobs in parallel when their inputs are ready, and caches outputs by content hash, so an unchanged job is skipped."),
    ("cache.txt", "The build cache. Outputs are stored by content hash in the cache folder. Cache entries expire after thirty days. Clearing the cache forces every job to run again; the disk space is reclaimed at once."),
    ("errors.txt", "Troubleshooting errors. A failing job prints its command and exit status. Missing inputs are reported before any job runs. If the cache folder is not writable, Kestrel warns and runs without caching."),
    ("faq.txt", "Frequently asked questions. Can jobs run on several machines? Not yet: remote execution is planned. Does Kestrel need a network? Only to download the installer. Where is the config file? Next to the project."),
]


class QueryError(Exception):
    pass


def stem(w):
    if len(w) > 5 and w.endswith("ing"):
        return w[:-3]
    if len(w) > 3 and w.endswith("s") and not w.endswith("ss"):
        return w[:-1]
    return w


def raw_words(text):
    out, cur = [], ""
    for c in text:
        if c.isalnum():
            cur += c
        elif cur:
            out.append(cur)
            cur = ""
    if cur:
        out.append(cur)
    return out


def term_of(word, stop):
    w = word.lower()
    if w in stop:
        return None
    return stem(w)


class DocIndex:
    def __init__(self, name, hash_, words, terms):
        self.name, self.hash, self.words, self.terms = name, hash_, words, terms


def fnv(text):
    h = 2166136261
    for b in text.encode():
        h ^= b
        h = (h * 16777619) % 4294967296
    return f"{h:08x}"


def index_doc(name, text, stop):
    terms = {}
    pos = 0
    for w in raw_words(text):
        t = term_of(w, stop)
        if t is not None:
            terms.setdefault(t, []).append(pos)
            pos += 1
    return DocIndex(name, fnv(text), pos, terms)


def index_all(jobs, stop):
    results = queue.Queue()

    def task(name, text):
        d = index_doc(name, text, stop)
        results.put(d)
        return d.words

    with ThreadPoolExecutor(max_workers=3) as pool:
        futures = [pool.submit(task, name, text) for name, text in jobs]
        total = sum(f.result() for f in futures)
    docs = []
    while not results.empty():
        docs.append(results.get())
    docs.sort(key=lambda d: d.name)
    return docs, total


def save(docs):
    out = f"kestrel-index 1 {len(docs)}\n"
    for d in docs:
        out += f"doc {d.name} {d.hash} {d.words} {len(d.terms)}\n"
        for t in sorted(d.terms):
            out += f"{t} {','.join(str(p) for p in d.terms[t])}\n"
    with open(INDEX, "w") as f:
        f.write(out)


def parse_int(s):
    return int(s) if s and all(c in "0123456789" for c in s.lstrip("-")) and s.lstrip("-") else None


def load():
    try:
        with open(INDEX) as f:
            text = f.read()
    except OSError:
        raise QueryError("cannot read search.idx")
    lines = text.splitlines()
    head = lines[0].split(" ")
    if len(head) != 3 or head[0] != "kestrel-index" or head[1] != "1":
        raise QueryError("line 1: not a kestrel index")
    count = parse_int(head[2])
    if count is None:
        raise QueryError("line 1: bad document count")
    docs = []
    at = 1
    for _ in range(count):
        f = lines[at].split(" ")
        if len(f) != 5 or f[0] != "doc":
            raise QueryError(f"line {at + 1}: expected a doc line")
        words = parse_int(f[3])
        if words is None:
            raise QueryError(f"line {at + 1}: bad word count")
        n = parse_int(f[4])
        if n is None:
            raise QueryError(f"line {at + 1}: bad term count")
        at += 1
        terms = {}
        for _ in range(n):
            if at >= len(lines):
                raise QueryError(f"line {at + 1}: index ends early")
            parts = lines[at].split(" ")
            if len(parts) != 2:
                raise QueryError(f"line {at + 1}: expected a term and its positions")
            ps = []
            for p in parts[1].split(","):
                v = parse_int(p)
                if v is None:
                    raise QueryError(f"line {at + 1}: bad position '{p}'")
                ps.append(v)
            terms[parts[0]] = ps
            at += 1
        docs.append(DocIndex(f[1], f[2], words, terms))
    return docs


# Items are (kind, value, negated, shown) with kind "term", "prefix" or
# "phrase"; a query is an AND of groups, each an OR of items.

def parse_query(q, stop):
    groups = []
    join_next = False
    i = 0
    while i < len(q):
        if q[i] == " ":
            i += 1
            continue
        negated = q[i] == "-"
        if negated:
            i += 1
        item = None
        if i < len(q) and q[i] == '"':
            start = i
            i += 1
            text = ""
            while i < len(q) and q[i] != '"':
                text += q[i]
                i += 1
            if i == len(q):
                raise QueryError(f"unterminated quote at {start}")
            i += 1
            words = [t for t in (term_of(w, stop) for w in raw_words(text)) if t is not None]
            if not words:
                raise QueryError(f'phrase "{text}" has only stopwords')
            item = ("phrase", words, negated, f'"{text}"')
        else:
            word = ""
            while i < len(q) and q[i] != " ":
                word += q[i]
                i += 1
            if word == "OR" and not negated:
                if not groups or join_next:
                    raise QueryError("OR needs a term on each side")
                join_next = True
                continue
            if word.endswith("*"):
                p = word[:-1].lower()
                if len(p) < 2:
                    raise QueryError(f"prefix '{word}' is too short")
                item = ("prefix", p, negated, word)
            else:
                t = term_of(word, stop)
                if t is not None:
                    item = ("term", t, negated, word)
        if item is not None:
            if join_next:
                if item[2]:
                    raise QueryError("cannot OR a negated term")
                groups[-1].append(item)
                join_next = False
            else:
                groups.append([item])
    if join_next:
        raise QueryError("OR needs a term on each side")
    if not groups:
        raise QueryError("nothing to search for")
    return groups


def hits(d, kind, value):
    if kind == "term":
        ps = d.terms.get(value)
        return (len(ps), ps[0]) if ps else (0, -1)
    if kind == "prefix":
        n, first = 0, -1
        for t in sorted(d.terms):
            if t.startswith(value):
                ps = d.terms[t]
                n += len(ps)
                if first < 0 or ps[0] < first:
                    first = ps[0]
        return n, first
    starts = d.terms.get(value[0])
    if starts is None:
        return 0, -1
    n, first = 0, -1
    for s in starts:
        ok = all(value[k] in d.terms and s + k in d.terms[value[k]] for k in range(1, len(value)))
        if ok:
            n += 1
            if first < 0:
                first = s
    return n, first


def search(docs, groups):
    n = len(docs)
    out = []
    for di in range(n):
        keep, score, first = True, 0, -1
        for g in groups:
            any_ = False
            for kind, value, negated, _ in g:
                tf, at = hits(docs[di], kind, value)
                if negated:
                    if tf > 0:
                        keep = False
                    any_ = True
                    continue
                if tf > 0:
                    any_ = True
                    df = sum(1 for d in docs if hits(d, kind, value)[0] > 0)
                    score += tf * (100 * n // df)
                    if first < 0 or at < first:
                        first = at
            if not any_:
                keep = False
        if keep:
            out.append((di, score, first))
    out.sort(key=lambda h: (-h[1], docs[h[0]].name))
    return out


def snippet(text, first, stop):
    words = raw_words(text)
    ordinal, at = 0, 0
    for i, w in enumerate(words):
        if term_of(w, stop) is not None:
            if ordinal == first:
                at = i
                break
            ordinal += 1
    lo = at - 3 if at > 3 else 0
    hi = at + 4 if at + 4 < len(words) else len(words)
    parts = ["..."] if lo > 0 else []
    for i in range(lo, hi):
        parts.append(f"[{words[i]}]" if i == at else words[i])
    if hi < len(words):
        parts.append("...")
    return " ".join(parts)


def text_of(name):
    try:
        with open(name) as f:
            return f.read()
    except OSError:
        return ""


def run_queries(docs, queries, stop):
    for q in queries:
        print(f"> {q}")
        try:
            groups = parse_query(q, stop)
        except QueryError as e:
            print(f"  error: {e}")
            continue
        found = search(docs, groups)
        if not found:
            print("  no matches")
        for di, score, first in found:
            d = docs[di]
            if first < 0:
                print(f"  {d.name} ({score})")
            else:
                print(f"  {d.name} ({score}): {snippet(text_of(d.name), first, stop)}")


def stats(docs):
    df = {}
    postings = 0
    for d in docs:
        for t, ps in d.terms.items():
            postings += len(ps)
            df[t] = df.get(t, 0) + 1
    common = sorted((-n, t) for t, n in df.items())
    top = " ".join(f"{t}:{-n}" for n, t in common[:5])
    print(f"  {len(docs)} documents, {len(df)} terms, {postings} postings; most common {top}")


def refresh(names, stop):
    try:
        old = load()
    except QueryError as e:
        print(f"  index unusable ({e}); rebuilding")
        old = []
    kept, jobs = [], []
    for name in names:
        text = text_of(name)
        h = fnv(text)
        if not any(d.name == name and d.hash == h for d in old):
            jobs.append((name, text))
    for d in old:
        if d.name not in names:
            print(f"  dropped {d.name}")
            continue
        if any(j[0] == d.name for j in jobs):
            continue
        kept.append(d)
    changed = [j[0] for j in jobs]
    which = ", ".join(changed) if changed else "none"
    print(f"  kept {len(kept)}, re-indexing {len(changed)}: {which}")
    fresh, total = index_all(jobs, stop)
    print(f"  indexed {total} terms")
    kept += fresh
    kept.sort(key=lambda d: d.name)
    return kept


def main():
    stop = set(STOPWORDS.split(" "))
    names, jobs = [], []
    for name, text in DOCS:
        with open(name, "w") as f:
            f.write(text)
        names.append(name)
        jobs.append((name, text_of(name)))

    print("== index")
    docs, total = index_all(jobs, stop)
    print(f"  indexed {total} terms")
    stats(docs)
    save(docs)
    print("  saved search.idx")

    print("== search")
    queries = [
        "cache",
        "jobs parallel",
        "config OR settings",
        "installer -linux",
        '"content hash"',
        '"disk space" cache',
        "instal*",
        "run*",
        "kestrel -jobs -cache",
        "the and of",
        '"is not"',
        "remote OR network OR offline",
        '"unterminated',
        "OR cache",
        "cache OR",
        "x*",
        "nothing-here",
    ]
    run_queries(docs, queries, stop)

    print("== documents change")
    with open("cache.txt", "w") as f:
        f.write("The build cache. Outputs are stored by content hash. Entries expire after seven days; a cleanup job removes them.")
    print("  rewrote cache.txt")
    with open("plugins.txt", "w") as f:
        f.write("Writing plugins. A plugin adds a job type. Plugins are loaded from the plugin folder at start and can be reloaded without a restart.")
    print("  added plugins.txt")
    names2 = [n for n in names if n != "faq.txt"] + ["plugins.txt"]
    print("  faq.txt is no longer part of the set")

    print("== restart")
    docs2 = refresh(names2, stop)
    stats(docs2)
    save(docs2)
    print("  saved search.idx")
    run_queries(docs2, ["cache expire*", "plugin* OR network", '"content hash"', "reload*"], stop)

    print("== damaged index")
    saved = text_of(INDEX)
    with open(INDEX, "w") as f:
        f.write(saved.replace("\nexpire ", "\nexpire x", 1))
    docs3 = refresh(names2, stop)
    stats(docs3)


main()
