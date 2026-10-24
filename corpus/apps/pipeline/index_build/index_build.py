"""Python mirror of index_build.kara: the same generator, parsers, index,
ranking and merge, statement for statement, so its output is the expected
one.

Run: python3 -I -B index_build.py > expected.out"""
import os

MASK32 = 4294967295
DOCS = 500
URLS = 1400
QUERIES = 24
K1 = 1200
B = 750
TOP = 5
HEADER = "id\turl\ttitle\tbody"

SYLLABLES = ["ka", "ri", "to", "men", "sa", "lo", "vi", "dan", "el", "mor", "ra", "ne",
             "jo", "lin", "ta", "bo", "qu", "est", "par", "ix"]
STOPWORDS = ["the", "a", "of", "and", "to", "in", "is", "for", "on", "with", "as", "by"]
SUFFIXES = ["", "", "", "", "s", "ing", "ed"]


class IndexError_(Exception):
    def __init__(self, what):
        self.what = what


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


# ------------------------------------------------------------ the data

def vocabulary():
    words = []
    for i in range(1200):
        h = hash32(i * 7919 + 17)
        n = 2 + h % 3
        w = ""
        for j in range(n):
            w += SYLLABLES[(h >> (5 * j + 2)) % 20]
        words.append(w)
    return words


class Doc:
    def __init__(self, did, url, title, body):
        self.did = did
        self.url = url
        self.title = title
        self.body = body


class Shard:
    def __init__(self, name, seed):
        self.name = name
        self.seed = seed


def pick_word(vocab, h):
    return vocab[((h % 1200) * ((h >> 11) % 1200)) // 1200]


def build_docs(shard, vocab):
    s = shard.seed
    out = []
    for i in range(DOCS):
        did = shard.name + "-" + str(i + 1).rjust(4, "0")
        url = "doc/" + str(mix(s, i, 1) % URLS)
        nt = 3 + mix(s, i, 2) % 4
        title = []
        for j in range(nt):
            w = pick_word(vocab, mix(s, i * 8 + j, 3))
            title.append(w[0].upper() + w[1:] if j == 0 else w)
        nb = 20 + mix(s, i, 4) % 81
        body = []
        for j in range(nb):
            h = mix(s, i * 128 + j, 5)
            if h % 5 == 0:
                body.append(STOPWORDS[(h >> 3) % 12])
            else:
                body.append(pick_word(vocab, h >> 3) + SUFFIXES[(h >> 20) % 7])
        out.append(Doc(did, url, " ".join(title), " ".join(body)))
    return out


def encode_docs(docs):
    lines = [HEADER]
    for d in docs:
        lines.append(d.did + "\t" + d.url + "\t" + d.title + "\t" + d.body)
    return "\n".join(lines) + "\n"


def read_text(path):
    try:
        with open(path) as f:
            return f.read()
    except OSError:
        raise IndexError_("could not read " + path)


def damage(text, n):
    lines = text.split("\n")
    lines[n + 1] = lines[n + 1].replace(" ", "\t", 1)
    return "\n".join(lines)


# ------------------------------------------------------------ the parsers

def is_digits(s):
    if len(s) == 0:
        return False
    for c in s:
        if c < "0" or c > "9":
            return False
    return True


def parse_docs(path, name, text):
    lines = text.split("\n")
    if len(lines) == 0 or lines[0] != HEADER:
        raise IndexError_(path + " line 1: bad header")
    out = []
    for n in range(1, len(lines)):
        line = lines[n]
        if line == "":
            continue
        where = path + " line " + str(n + 1) + ": "
        f = line.split("\t")
        if len(f) != 4:
            raise IndexError_(where + str(len(f)) + " fields")
        prefix = name + "-"
        if not f[0].startswith(prefix) or not is_digits(f[0][len(prefix):]):
            raise IndexError_(where + "bad id '" + f[0] + "'")
        if not f[1].startswith("doc/") or not is_digits(f[1][4:]):
            raise IndexError_(where + "bad url '" + f[1] + "'")
        if f[2] == "" or f[3] == "":
            raise IndexError_(where + "empty title or body")
        out.append(Doc(f[0], f[1], f[2], f[3]))
    return out


def same_docs(a, b):
    if len(a) != len(b):
        return False
    for x, y in zip(a, b):
        if (x.did, x.url, x.title, x.body) != (y.did, y.url, y.title, y.body):
            return False
    return True


# ------------------------------------------------------------ analysis

class Stats:
    def __init__(self):
        self.tokens = 0
        self.stopped = 0
        self.stem_s = 0
        self.stem_ing = 0
        self.stem_ed = 0


def stem(w, st):
    if len(w) > 5 and w.endswith("ing"):
        st.stem_ing += 1
        return w[:-3]
    if len(w) > 4 and w.endswith("ed"):
        st.stem_ed += 1
        return w[:-2]
    if len(w) > 3 and w.endswith("s") and not w.endswith("ss"):
        st.stem_s += 1
        return w[:-1]
    return w


def tokenize(text, st):
    out = []
    word = ""
    for c in text.lower() + " ":
        if ("a" <= c <= "z") or ("0" <= c <= "9"):
            word += c
        elif word != "":
            st.tokens += 1
            if word in STOPWORDS:
                st.stopped += 1
            else:
                out.append(stem(word, st))
            word = ""
    return out


class Index:
    def __init__(self):
        self.terms = []
        self.postings = []   # per term: list of (doc, tf)
        self.lengths = []
        self.total = 0
        self.stats = Stats()


def build_index(docs):
    ix = Index()
    triples = []
    for k in range(len(docs)):
        d = docs[k]
        toks = tokenize(d.title + " " + d.body, ix.stats)
        ix.lengths.append(len(toks))
        ix.total += len(toks)
        toks.sort()
        i = 0
        while i < len(toks):
            j = i
            while j < len(toks) and toks[j] == toks[i]:
                j += 1
            triples.append((toks[i], k, j - i))
            i = j
    triples.sort()
    for term, k, tf in triples:
        if len(ix.terms) == 0 or ix.terms[-1] != term:
            ix.terms.append(term)
            ix.postings.append([])
        ix.postings[-1].append((k, tf))
    return ix


DIGITS = "0123456789abcdefghijklmnopqrstuvwxyz"


def base36(n):
    if n == 0:
        return "0"
    s = ""
    while n > 0:
        s = DIGITS[n % 36] + s
        n //= 36
    return s


def encode_index(ix):
    lines = ["#index " + str(len(ix.lengths)) + " " + str(len(ix.terms))]
    lines.append("#lengths " + " ".join(base36(n) for n in ix.lengths))
    for t in range(len(ix.terms)):
        last = 0
        parts = []
        for k, tf in ix.postings[t]:
            parts.append(base36(k - last) + ":" + base36(tf))
            last = k
        lines.append(ix.terms[t] + "\t" + str(len(ix.postings[t])) + "\t" + " ".join(parts))
    return "\n".join(lines) + "\n"


def from_base36(s, where):
    if s == "":
        raise IndexError_(where + "empty number")
    n = 0
    for c in s:
        d = DIGITS.find(c)
        if d < 0:
            raise IndexError_(where + "bad digit '" + c + "'")
        n = n * 36 + d
    return n


def parse_index(path, text):
    lines = text.split("\n")
    head = lines[0].split(" ")
    if len(head) != 3 or head[0] != "#index" or not is_digits(head[1]) or not is_digits(head[2]):
        raise IndexError_(path + " line 1: not an index")
    ndocs = int(head[1])
    nterms = int(head[2])
    ix = Index()
    lens = lines[1].split(" ")
    if lens[0] != "#lengths" or len(lens) != ndocs + 1:
        raise IndexError_(path + " line 2: bad lengths")
    for x in lens[1:]:
        n = from_base36(x, path + " line 2: ")
        ix.lengths.append(n)
        ix.total += n
    for n in range(2, len(lines)):
        line = lines[n]
        if line == "":
            continue
        where = path + " line " + str(n + 1) + ": "
        f = line.split("\t")
        if len(f) != 3 or not is_digits(f[1]):
            raise IndexError_(where + "bad entry")
        df = int(f[1])
        parts = f[2].split(" ")
        if len(parts) != df:
            raise IndexError_(where + "df " + str(df) + " but " + str(len(parts)) + " postings")
        plist = []
        last = 0
        for p in parts:
            dk, _, tf = p.partition(":")
            last += from_base36(dk, where)
            plist.append((last, from_base36(tf, where)))
        if len(ix.terms) > 0 and ix.terms[-1] >= f[0]:
            raise IndexError_(where + "terms out of order")
        ix.terms.append(f[0])
        ix.postings.append(plist)
    if len(ix.terms) != nterms:
        raise IndexError_(path + ": " + str(nterms) + " terms declared, " + str(len(ix.terms)) + " found")
    return ix


def same_index(a, b):
    return a.terms == b.terms and a.postings == b.postings and a.lengths == b.lengths


def find_term(ix, term):
    lo = 0
    hi = len(ix.terms)
    while lo < hi:
        mid = (lo + hi) // 2
        if ix.terms[mid] < term:
            lo = mid + 1
        else:
            hi = mid
    if lo < len(ix.terms) and ix.terms[lo] == term:
        return lo
    return -1


# ------------------------------------------------------------ ranking

def log2_fixed(x):
    """log2(x / 1000) in units of 1/1024, for x >= 1000."""
    k = 0
    while x >= 2000:
        x //= 2
        k += 1
    r = k * 1024
    bit = 512
    for _ in range(10):
        x = x * x // 1000
        if x >= 2000:
            x //= 2
            r += bit
        bit //= 2
    return r


def idf(n, df):
    return log2_fixed(((n - df) * 2 + 1) * 1000 // (df * 2 + 1) + 1000)


def make_queries(shard, vocab):
    out = []
    for q in range(QUERIES):
        h = mix(shard.seed, q, 9)
        n = 1 + h % 3
        words = []
        for j in range(n):
            words.append(pick_word(vocab, mix(shard.seed, q * 4 + j, 10)) + SUFFIXES[(h >> (3 * j + 4)) % 7])
        out.append(" ".join(words))
    return out


def search(ix, query):
    st = Stats()
    terms = tokenize(query, st)
    n = len(ix.lengths)
    avg = ix.total * 1000 // n
    scores = [0] * n
    hit = [False] * n
    for term in terms:
        t = find_term(ix, term)
        if t < 0:
            continue
        w = idf(n, len(ix.postings[t]))
        for k, tf in ix.postings[t]:
            norm = 1000 - B + B * ix.lengths[k] * 1000 // avg
            scores[k] += w * tf * (K1 + 1000) // (tf * 1000 + K1 * norm // 1000)
            hit[k] = True
    ranked = [k for k in range(n) if hit[k]]
    ranked.sort(key=lambda k: (-scores[k], k))
    return [(k, scores[k]) for k in ranked[:TOP]], len(ranked)


def run_queries(ix, queries):
    return [search(ix, q) for q in queries]


# ------------------------------------------------------------ reports

def totals_report(shard, docs, ix):
    raw = sum(len(d.title) + len(d.body) for d in docs)
    postings = sum(len(p) for p in ix.postings)
    return (shard.name + " totals: " + str(len(docs)) + " documents, " + str(ix.stats.tokens) + " tokens ("
            + str(ix.stats.stopped) + " stopwords), " + str(len(ix.terms)) + " terms, " + str(postings)
            + " postings, mean length " + str(ix.total // len(docs)) + ", text " + str(raw) + " bytes")


def terms_report(shard, ix):
    order = sorted(range(len(ix.terms)), key=lambda t: (-len(ix.postings[t]), ix.terms[t]))
    tops = [ix.terms[t] + " " + str(len(ix.postings[t])) for t in order[:5]]
    once = sum(1 for p in ix.postings if len(p) == 1)
    return shard.name + " terms: most frequent " + ", ".join(tops) + "; " + str(once) + " in one document"


def query_report(shard, docs, queries, results):
    h = 2166136261
    hits = 0
    for top, total in results:
        hits += total
        for k, s in top:
            h = ((h ^ (k * 65536 + s % 65536)) * 16777619) & MASK32
    lines = [shard.name + " queries: " + str(len(queries)) + " run, " + str(hits) + " matching documents, results fnv "
             + format(h, "08x")]
    for q in range(3):
        top, total = results[q]
        shown = ", ".join(docs[k].did + " " + str(s) for k, s in top[:3])
        lines.append("  '" + queries[q] + "': " + str(total) + " matches; " + (shown if shown else "none"))
    return "\n".join(lines)


def length_report(shard, ix):
    buckets = [0] * 6
    for n in ix.lengths:
        b = n // 20
        if b > 5:
            b = 5
        buckets[b] += 1
    parts = []
    for b in range(6):
        label = str(b * 20) + "-" + str(b * 20 + 19) if b < 5 else "100+"
        parts.append(label + ":" + str(buckets[b]))
    longest = max(range(len(ix.lengths)), key=lambda k: (ix.lengths[k], -k))
    return shard.name + " lengths: " + " ".join(parts) + "; longest " + str(ix.lengths[longest]) + " terms"


def stem_report(shard, ix, index_bytes):
    st = ix.stats
    raw = 0
    for t in range(len(ix.terms)):
        raw += len(ix.postings[t]) * 8
    return (shard.name + " analysis: stemmed " + str(st.stem_s) + " -s, " + str(st.stem_ing) + " -ing, "
            + str(st.stem_ed) + " -ed; index file " + str(index_bytes) + " bytes for " + str(raw)
            + " bytes of fixed-width postings")


# ------------------------------------------------------------ the merge

class Live:
    def __init__(self, url, shard, did):
        self.url = url
        self.shard = shard
        self.did = did


class Catalog:
    def __init__(self):
        self.urls = []      # sorted
        self.live = []      # parallel to urls
        self.term_ids = {}
        self.next_term = 1
        self.log = ""


def find_url(cat, url):
    lo = 0
    hi = len(cat.urls)
    while lo < hi:
        mid = (lo + hi) // 2
        if cat.urls[mid] < url:
            lo = mid + 1
        else:
            hi = mid
    return lo


def merge(cat, shard, docs, ix):
    fresh = 0
    replaced = 0
    first = ""
    for d in docs:
        at = find_url(cat, d.url)
        if at < len(cat.urls) and cat.urls[at] == d.url:
            old = cat.live[at]
            if replaced == 0:
                first = d.url + " (was " + old.did + ")"
            replaced += 1
            cat.live[at] = Live(d.url, shard.name, d.did)
            cat.log += "replace " + d.url + " " + old.did + " " + d.did + "\n"
        else:
            cat.urls.insert(at, d.url)
            cat.live.insert(at, Live(d.url, shard.name, d.did))
            fresh += 1
            cat.log += "add " + d.url + " " + d.did + "\n"
    lo = cat.next_term
    for term in ix.terms:
        if term not in cat.term_ids:
            cat.term_ids[term] = cat.next_term
            cat.next_term += 1
    new_terms = cat.next_term - lo
    ids = "ids " + str(lo) + "-" + str(cat.next_term - 1) if new_terms > 0 else "no new ids"
    rep = "first " + first if replaced > 0 else "none replaced"
    return (shard.name + " merged: " + str(fresh) + " new, " + str(replaced) + " replace older copies (" + rep + "); "
            + str(new_terms) + " new terms, " + ids + "; catalog " + str(len(cat.urls)) + " urls")


# ------------------------------------------------------------ the job

def run():
    vocab = vocabulary()
    news = Shard("news", 11)
    docs = Shard("docs", 23)
    forum = Shard("forum", 37)
    wiki = Shard("wiki", 41)

    d1 = build_docs(news, vocab)
    d2 = build_docs(docs, vocab)
    d3 = build_docs(forum, vocab)
    d4 = build_docs(wiki, vocab)

    with open("news.tsv", "w") as f:
        f.write(encode_docs(d1))
    with open("docs.tsv", "w") as f:
        f.write(encode_docs(d2))
    with open("forum.tsv", "w") as f:
        f.write(encode_docs(d3))
    with open("wiki.tsv", "w") as f:
        f.write(encode_docs(d4))

    whole = read_text("forum.tsv")
    with open("forum.bad", "w") as f:
        f.write(damage(whole, 200))

    t1 = read_text("news.tsv")
    t2 = read_text("docs.tsv")
    t3 = read_text("forum.tsv")
    t4 = read_text("wiki.tsv")
    for path, t in [("news.tsv", t1), ("docs.tsv", t2), ("forum.tsv", t3), ("wiki.tsv", t4)]:
        print(path + ": " + str(len(t)) + " bytes, fnv " + format(text_sum(t), "08x"))

    g1 = parse_docs("news.tsv", "news", t1)
    g2 = parse_docs("docs.tsv", "docs", t2)
    g3 = parse_docs("forum.tsv", "forum", t3)
    g4 = parse_docs("wiki.tsv", "wiki", t4)
    try:
        parse_docs("forum.bad", "forum", read_text("forum.bad"))
        print("forum.bad: parsed")
    except IndexError_ as e:
        print(e.what)
    try:
        parse_docs("wiki.tsv", "news", t4)
        print("wiki.tsv as news: parsed")
    except IndexError_ as e:
        print(e.what)
    same = same_docs(g1, d1) and same_docs(g2, d2) and same_docs(g3, d3) and same_docs(g4, d4)
    print("parsed documents match the built ones: " + str(same).lower())

    x1 = build_index(g1)
    x2 = build_index(g2)
    x3 = build_index(g3)
    x4 = build_index(g4)

    with open("news.idx", "w") as f:
        f.write(encode_index(x1))
    with open("docs.idx", "w") as f:
        f.write(encode_index(x2))
    with open("forum.idx", "w") as f:
        f.write(encode_index(x3))
    with open("wiki.idx", "w") as f:
        f.write(encode_index(x4))

    i1 = read_text("news.idx")
    i2 = read_text("docs.idx")
    i3 = read_text("forum.idx")
    i4 = read_text("wiki.idx")
    for path, t in [("news.idx", i1), ("docs.idx", i2), ("forum.idx", i3), ("wiki.idx", i4)]:
        print(path + ": " + str(len(t)) + " bytes, fnv " + format(text_sum(t), "08x"))

    y1 = parse_index("news.idx", i1)
    y2 = parse_index("docs.idx", i2)
    y3 = parse_index("forum.idx", i3)
    y4 = parse_index("wiki.idx", i4)
    try:
        parse_index("news.tsv", t1)
        print("news.tsv as an index: parsed")
    except IndexError_ as e:
        print(e.what)
    same_x = same_index(y1, x1) and same_index(y2, x2) and same_index(y3, x3) and same_index(y4, x4)
    print("parsed indexes match the built ones: " + str(same_x).lower())

    q1 = make_queries(news, vocab)
    q2 = make_queries(docs, vocab)
    q3 = make_queries(forum, vocab)
    q4 = make_queries(wiki, vocab)

    r1 = run_queries(y1, q1)
    r2 = run_queries(y2, q2)
    r3 = run_queries(y3, q3)
    r4 = run_queries(y4, q4)

    reports = [totals_report(news, g1, x1), totals_report(docs, g2, x2), totals_report(forum, g3, x3),
               totals_report(wiki, g4, x4),
               terms_report(news, y1), terms_report(docs, y2), terms_report(forum, y3), terms_report(wiki, y4),
               query_report(news, g1, q1, r1), query_report(docs, g2, q2, r2), query_report(forum, g3, q3, r3),
               query_report(wiki, g4, q4, r4),
               length_report(news, y1), length_report(docs, y2), length_report(forum, y3), length_report(wiki, y4),
               stem_report(news, x1, len(i1)), stem_report(docs, x2, len(i2)), stem_report(forum, x3, len(i3)),
               stem_report(wiki, x4, len(i4))]
    for line in reports:
        print(line)

    cat = Catalog()
    print(merge(cat, news, g1, y1))
    print(merge(cat, docs, g2, y2))
    print(merge(cat, forum, g3, y3))
    print(merge(cat, wiki, g4, y4))
    with open("catalog.log", "w") as f:
        f.write(cat.log)
    back = read_text("catalog.log")
    by_shard = {"news": 0, "docs": 0, "forum": 0, "wiki": 0}
    for live in cat.live:
        by_shard[live.shard] += 1
    rows = len(back.split("\n")) - 1
    print("catalog.log: " + str(rows) + " entries, live news " + str(by_shard["news"]) + ", docs "
          + str(by_shard["docs"]) + ", forum " + str(by_shard["forum"]) + ", wiki " + str(by_shard["wiki"])
          + ", " + str(cat.next_term - 1) + " terms, " + str(len(back)) + " bytes, fnv " + format(text_sum(back), "08x"))

    for p in ["news.tsv", "docs.tsv", "forum.tsv", "wiki.tsv", "forum.bad", "news.idx", "docs.idx", "forum.idx",
              "wiki.idx", "catalog.log"]:
        os.remove(p)


def main():
    try:
        run()
    except IndexError_ as e:
        print("index_build failed: " + e.what)


main()
