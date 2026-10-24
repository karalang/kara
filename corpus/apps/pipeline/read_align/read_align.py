"""Python mirror of read_align.kara, statement for statement. Integer
arithmetic only, and every division is of non-negative values, so Python's
// is Kara's /.

Run: python3 -I -B read_align.py > expected.out"""
import os

MASK32 = 4294967295
BASE_LEN = 5600
ELEM_LEN = 300
DUP_LEN = 120
READ_LEN = 60
COVERAGE = 8
K = 12
SEED_STEP = 12
BAND = 8
MATCH = 2
MISMATCH = -3
GAP = -5
NEG = -1000000000
MIN_SCORE = 60
MIN_MAPQ = 20
MIN_QUAL = 15
POS_KEY = 8192
SPACING = 80
BASES = "ACGT"


class Bad(Exception):
    pass


class Ref:
    def __init__(self, name, seq, elem_at, dup_from, dup_at):
        self.name, self.seq = name, seq
        self.elem_at, self.dup_from, self.dup_at = elem_at, dup_from, dup_at


class Donor:
    def __init__(self, seq, origin, truth):
        self.seq, self.origin, self.truth = seq, origin, truth


class Read:
    def __init__(self, name, seq, qual, truth, strand):
        self.name, self.seq, self.qual = name, seq, qual
        self.truth, self.strand = truth, strand


class Aln:
    def __init__(self):
        self.mapped = False
        self.rev = False
        self.pos = 0
        self.score = 0
        self.mapq = 0
        self.ops = []
        self.lens = []
        self.nm = 0
        self.seq = []
        self.qual = []


class Call:
    def __init__(self, pos, ref, alt, gt, depth, ac):
        self.pos, self.ref, self.alt, self.gt = pos, ref, alt, gt
        self.depth, self.ac = depth, ac

    def text(self):
        return f"{self.pos} {self.ref} {self.alt}"


class Entry:
    def __init__(self, sig, id_, chrom, pos):
        self.sig, self.id, self.chrom, self.pos = sig, id_, chrom, pos
        self.samples = 1


# ------------------------------------------------------------ the genomes

def hash32(a):
    h = a & MASK32
    h = (((h >> 16) ^ h) * 73244475) & MASK32
    h = (((h >> 16) ^ h) * 73244475) & MASK32
    return (h >> 16) ^ h


def mix(seed, i, salt):
    return hash32(seed * 1000003 + i * 104729 + salt * 15485863)


def random_bases(seed, n, salt):
    return [mix(seed, i, salt) % 4 for i in range(n)]


def bases_text(seq, lo, hi):
    return "".join(BASES[seq[i]] for i in range(lo, hi))


# A chromosome: random sequence with a mobile element shared by all four
# chromosomes, and one segment of its own copied a second time.
def build_ref(name, seed, element):
    rnd = random_bases(seed, BASE_LEN, 1)
    elem_at = 1200 + mix(seed, 0, 2) % 400
    dup_from = 2800 + mix(seed, 0, 3) % 400
    dup_at = dup_from + 900 + mix(seed, 0, 4) % 300
    seq = []
    for i in range(BASE_LEN):
        if i == elem_at:
            seq.extend(element)
        if i == dup_at:
            for j in range(dup_from, dup_from + DUP_LEN):
                seq.append(rnd[j])
        seq.append(rnd[i])
    dup_from_ref = dup_from + ELEM_LEN
    dup_at_ref = dup_at + ELEM_LEN
    return Ref(name, seq, elem_at, dup_from_ref, dup_at_ref)


# ------------------------------------------------------------ the donors

def far_enough(taken, p):
    for t in taken:
        if abs(t - p) < SPACING:
            return False
    return True


def in_element(r, p):
    return r.elem_at - 20 <= p < r.elem_at + ELEM_LEN + 20


# A deletion of seq[p..p+n) can slide left when seq[p-1] == seq[p+n-1];
# an insertion s before p when s's last base is seq[p-1]. Planted indels
# are moved right until they cannot, so the truth is left-aligned.
def settle_del(seq, p, n):
    while seq[p - 1] == seq[p + n - 1]:
        p += 1
    return p


def settle_ins(seq, p, s):
    while s[len(s) - 1] == seq[p - 1]:
        p += 1
    return p


# kind 0 = SNP (alt base), 1 = deletion (length), 2 = insertion (bases).
def plan_variants(r, seed):
    seq = r.seq
    n = len(seq)
    kinds, poss, alts = [], [], []
    taken = []
    # The two shared variants inside the element.
    p = r.elem_at + 150
    kinds.append(0)
    poss.append(p)
    alts.append([(seq[p] + 2) % 4])
    taken.append(p)
    p = settle_del(seq, r.elem_at + 230, 2)
    kinds.append(1)
    poss.append(p)
    alts.append([2])
    taken.append(p)
    salt = 10
    for want in range(14):
        kind = 0
        if want >= 10:
            kind = 1 if want < 12 else 2
        while True:
            h = mix(seed, want, salt)
            salt += 1
            p = 100 + h % (n - 200)
            if not far_enough(taken, p) or in_element(r, p):
                continue
            if kind == 0:
                alts.append([(seq[p] + 1 + (h // 7) % 3) % 4])
            elif kind == 1:
                ln = 1 + (h // 7) % 3
                p = settle_del(seq, p, ln)
                alts.append([ln])
            else:
                ln = 1 + (h // 7) % 3
                s = [mix(seed, want, salt + 100 + j) % 4 for j in range(ln)]
                p = settle_ins(seq, p, s)
                alts.append(s)
            if not far_enough(taken, p):
                alts.pop()
                continue
            kinds.append(kind)
            poss.append(p)
            taken.append(p)
            break
    # Sorted by position (an insertion sorts before p, so the order is
    # the walk's).
    order = sorted(range(len(poss)), key=lambda i: poss[i] * 4 + kinds[i])
    return [kinds[i] for i in order], [poss[i] for i in order], [alts[i] for i in order]


def truth_text(seq, kind, p, alt):
    if kind == 0:
        return f"{p + 1} {BASES[seq[p]]} {BASES[alt[0]]}"
    if kind == 1:
        return f"{p} {bases_text(seq, p - 1, p + alt[0])} {BASES[seq[p - 1]]}"
    ins = "".join(BASES[b] for b in alt)
    return f"{p} {BASES[seq[p - 1]]} {BASES[seq[p - 1]]}{ins}"


def make_donor(r, seed):
    kinds, poss, alts = plan_variants(r, seed)
    seq = r.seq
    out, origin, truth = [], [], []
    v = 0
    i = 0
    while i < len(seq):
        if v < len(poss) and poss[v] == i:
            truth.append(truth_text(seq, kinds[v], i, alts[v]))
            k = kinds[v]
            a = alts[v]
            v += 1
            if k == 0:
                out.append(a[0])
                origin.append(i)
                i += 1
                continue
            if k == 1:
                i += a[0]
                continue
            for b in a:
                out.append(b)
                origin.append(i)
        out.append(seq[i])
        origin.append(i)
        i += 1
    return Donor(out, origin, truth)


# ------------------------------------------------------------ sequencing

def revcomp(seq):
    return [3 - seq[len(seq) - 1 - i] for i in range(len(seq))]


def reverse(v):
    return [v[len(v) - 1 - i] for i in range(len(v))]


def sequence(name, d, seed):
    count = len(d.seq) * COVERAGE // READ_LEN
    reads = []
    for r in range(count):
        h = mix(seed, r, 50)
        start = h % (len(d.seq) - READ_LEN + 1)
        strand = (h // 7919) % 2
        seq, qual = [], []
        for j in range(READ_LEN):
            b = d.seq[start + j]
            e = mix(seed, r * READ_LEN + j, 51)
            if e % 1000 < 8:
                seq.append((b + 1 + (e // 1000) % 3) % 4)
                qual.append(8 + (e // 3000) % 10)
            else:
                seq.append(b)
                qual.append(25 + (e // 1000) % 15)
        if strand == 1:
            seq = revcomp(seq)
            qual = reverse(qual)
        rname = f"{name}.{r + 1:04}"
        reads.append(Read(rname, seq, qual, d.origin[start], strand))
    return reads


# ------------------------------------------------------------ files

def encode_fasta(r):
    out = [f">{r.name} length={len(r.seq)}\n"]
    i = 0
    while i < len(r.seq):
        hi = min(i + 60, len(r.seq))
        out.append(bases_text(r.seq, i, hi))
        out.append("\n")
        i = hi
    return "".join(out)


def encode_fastq(reads):
    out = []
    for rd in reads:
        sign = "-" if rd.strand == 1 else "+"
        out.append(f"@{rd.name} pos={rd.truth + 1} strand={sign}\n")
        out.append(bases_text(rd.seq, 0, len(rd.seq)))
        out.append("\n+\n")
        out.append("".join(chr(33 + q) for q in rd.qual))
        out.append("\n")
    return "".join(out)


def text_sum(text):
    h = 2166136261
    for b in text.encode():
        h = ((h ^ b) * 16777619) & MASK32
    return h


def read_text(path):
    try:
        with open(path) as f:
            return f.read()
    except OSError:
        raise Bad(f"could not read {path}")


def write_text(path, text):
    try:
        with open(path, "w") as f:
            f.write(text)
        return True
    except OSError:
        return False


# Cut the quality line of the n-th record short by one character.
def damage(text, n):
    lines = text.split("\n")
    q = 4 * n + 3
    lines[q] = lines[q][:len(lines[q]) - 1]
    return "\n".join(lines)


def base_code(c):
    return "ACGT".find(c)


def parse_int(field, what):
    if field == "" or not all("0" <= c <= "9" for c in field):
        raise Bad(f"{what}: bad number {field}")
    return int(field)


def parse_fasta(name, text):
    lines = text.split("\n")
    if len(lines) == 0 or not lines[0].startswith(">"):
        raise Bad(f"{name}.fa: no FASTA header")
    head = lines[0][1:].split(" ")
    if len(head) != 2 or not head[1].startswith("length="):
        raise Bad(f"{name}.fa: bad header {lines[0]}")
    if head[0] != name:
        raise Bad(f"{name}.fa: expected {name}, the file is {head[0]}")
    length = parse_int(head[1][7:], f"{name}.fa: line 1")
    seq = []
    for i in range(1, len(lines)):
        line = lines[i]
        if line == "":
            continue
        for c in line:
            b = base_code(c)
            if b < 0:
                raise Bad(f"{name}.fa: line {i + 1}: bad base {ord(c)}")
            seq.append(b)
    if len(seq) != length:
        raise Bad(f"{name}.fa: header says {length} bases, found {len(seq)}")
    return seq


def parse_fastq(name, text):
    lines = text.split("\n")
    if len(lines) > 0 and lines[len(lines) - 1] == "":
        lines.pop()
    if len(lines) % 4 != 0:
        raise Bad(f"{name}.fq: {len(lines)} lines, not whole records")
    reads = []
    for r in range(len(lines) // 4):
        head = lines[4 * r]
        bases = lines[4 * r + 1]
        plus = lines[4 * r + 2]
        quals = lines[4 * r + 3]
        rec = r + 1
        if not head.startswith("@") or not plus.startswith("+"):
            raise Bad(f"{name}.fq: record {rec}: not a FASTQ record")
        f = head[1:].split(" ")
        if len(f) != 3 or not f[1].startswith("pos=") or not f[2].startswith("strand="):
            raise Bad(f"{name}.fq: record {rec}: bad header")
        if len(bases) != len(quals):
            raise Bad(f"{name}.fq: record {rec}: {len(bases)} bases but {len(quals)} qualities")
        seq, qual = [], []
        for c in bases:
            b = base_code(c)
            if b < 0:
                raise Bad(f"{name}.fq: record {rec}: bad base {ord(c)}")
            seq.append(b)
        for c in quals:
            q = ord(c) - 33
            if q < 0 or q > 41:
                raise Bad(f"{name}.fq: record {rec}: bad quality {ord(c)}")
            qual.append(q)
        pos = parse_int(f[1][4:], f"{name}.fq: record {rec}")
        strand = 1 if f[2] == "strand=-" else 0
        reads.append(Read(f[0], seq, qual, pos - 1, strand))
    return reads


def same_reads(a, b):
    if len(a) != len(b):
        return False
    for i in range(len(a)):
        x, y = a[i], b[i]
        if x.name != y.name or x.seq != y.seq or x.qual != y.qual or x.truth != y.truth or x.strand != y.strand:
            return False
    return True


# ------------------------------------------------------------ the index

def kmer(seq, at):
    c = 0
    for j in range(K):
        c = c * 4 + seq[at + j]
    return c


def build_index(seq):
    idx = []
    for p in range(len(seq) - K + 1):
        idx.append(kmer(seq, p) * POS_KEY + p)
    idx.sort()
    return idx


def lower_bound(idx, key):
    lo, hi = 0, len(idx)
    while lo < hi:
        mid = (lo + hi) // 2
        if idx[mid] < key:
            lo = mid + 1
        else:
            hi = mid
    return lo


# ------------------------------------------------------------ alignment

# The diagonals the seeds vote for, best first, at most three, each more
# than BAND from the ones before.
def candidates(idx, read):
    diags = []
    off = 0
    while off + K <= len(read):
        code = kmer(read, off)
        i = lower_bound(idx, code * POS_KEY)
        while i < len(idx) and idx[i] // POS_KEY == code:
            diags.append(idx[i] % POS_KEY - off)
            i += 1
        off += SEED_STEP
    diags.sort()
    dv, cv = [], []
    for d in diags:
        if len(dv) > 0 and dv[len(dv) - 1] == d:
            cv[len(cv) - 1] += 1
        else:
            dv.append(d)
            cv.append(1)
    picked = []
    used = [False] * len(dv)
    while len(picked) < 3:
        best = -1
        for i in range(len(dv)):
            if not used[i] and (best < 0 or cv[i] > cv[best]):
                best = i
        if best < 0:
            break
        used[best] = True
        ok = True
        for p in picked:
            if abs(p - dv[best]) <= BAND:
                ok = False
        if ok:
            picked.append(dv[best])
    return picked


# Banded alignment of the whole read against the reference around diagonal
# d, free at both ends of the reference. Cell (i, k) ends the first i read
# bases at reference position d + i + k - BAND.
def align_at(ref, read, d, out):
    n = len(read)
    L = len(ref)
    w = 2 * BAND + 1
    h = [NEG] * ((n + 1) * w)
    tb = [0] * ((n + 1) * w)
    for k in range(w):
        j = d + k - BAND
        if 0 <= j <= L:
            h[k] = 0
    for i in range(1, n + 1):
        row = i * w
        prev = (i - 1) * w
        for k in range(w):
            j = d + i + k - BAND
            if j < 0 or j > L:
                continue
            best = NEG
            dir_ = 0
            if h[prev + k] > NEG:
                s = MATCH if read[i - 1] == ref[j - 1] else MISMATCH
                best = h[prev + k] + s
            if k + 1 < w and h[prev + k + 1] > NEG:
                v = h[prev + k + 1] + GAP
                if v > best:
                    best = v
                    dir_ = 1
            if k >= 1 and h[row + k - 1] > NEG:
                v = h[row + k - 1] + GAP
                if v > best:
                    best = v
                    dir_ = 2
            h[row + k] = best
            tb[row + k] = dir_
    kb = -1
    for k in range(w):
        if h[n * w + k] > NEG and (kb < 0 or h[n * w + k] > h[n * w + kb]):
            kb = k
    if kb < 0:
        return NEG
    score = h[n * w + kb]
    # The traceback, end to start.
    rops = []
    i = n
    k = kb
    nm = 0
    while i > 0:
        dir_ = tb[i * w + k]
        if dir_ == 0:
            j = d + i + k - BAND
            if read[i - 1] != ref[j - 1]:
                nm += 1
            rops.append(0)
            i -= 1
        elif dir_ == 1:
            rops.append(1)
            nm += 1
            i -= 1
            k += 1
        else:
            rops.append(2)
            nm += 1
            k -= 1
    out.pos = d + k - BAND
    out.ops, out.lens = [], []
    for t in range(len(rops) - 1, -1, -1):
        op = rops[t]
        if len(out.ops) > 0 and out.ops[len(out.ops) - 1] == op:
            out.lens[len(out.lens) - 1] += 1
        else:
            out.ops.append(op)
            out.lens.append(1)
    out.nm = nm
    out.score = score
    return score


def align_read(ref, idx, rd):
    best = Aln()
    best.score = NEG
    second = NEG
    found = 0
    for strand in range(2):
        seq = rd.seq if strand == 0 else revcomp(rd.seq)
        qual = rd.qual if strand == 0 else reverse(rd.qual)
        for d in candidates(idx, seq):
            if d + len(seq) < 0 or d > len(ref):
                continue
            a = Aln()
            s = align_at(ref, seq, d, a)
            if s == NEG:
                continue
            found += 1
            if s > best.score:
                second = best.score
                a.rev = strand == 1
                a.seq = seq
                a.qual = qual
                best = a
            elif s > second:
                second = s
    if found == 0 or best.score < MIN_SCORE:
        out = Aln()
        out.score = best.score if found > 0 else 0
        return out
    best.mapped = True
    if second == NEG:
        best.mapq = 60
    else:
        best.mapq = min(60, 4 * (best.score - second))
    return best


def align_all(ref, idx, reads):
    return [align_read(ref, idx, rd) for rd in reads]


# ------------------------------------------------------------ calling

def genotype(ac, depth):
    return "1/1" if ac * 10 >= depth * 8 else "0/1"


def call_variants(ref, alns):
    L = len(ref)
    counts = [0] * (L * 5)
    dels, inss = [], []
    for a in alns:
        if not a.mapped or a.mapq < MIN_MAPQ:
            continue
        j = a.pos
        q = 0
        for t in range(len(a.ops)):
            op, ln = a.ops[t], a.lens[t]
            if op == 0:
                for _ in range(ln):
                    if a.qual[q] >= MIN_QUAL:
                        counts[j * 5 + a.seq[q]] += 1
                    j += 1
                    q += 1
            elif op == 2:
                if ln <= 15 and j > 0:
                    dels.append(j * 16 + ln)
                for _ in range(ln):
                    counts[j * 5 + 4] += 1
                    j += 1
            else:
                if ln <= 8 and j > 0:
                    code = 0
                    for x in range(ln):
                        code = code * 4 + a.seq[q + x]
                    inss.append((j * 16 + ln) * 65536 + code)
                q += ln
    dels.sort()
    inss.sort()
    calls = []
    di, ii = 0, 0
    for p in range(1, L + 1):
        j = p - 1
        depth = sum(counts[j * 5:j * 5 + 5])
        r = ref[j]
        alt, ac = -1, 0
        for b in range(4):
            if b != r and counts[j * 5 + b] > ac:
                alt, ac = b, counts[j * 5 + b]
        if depth >= 4 and ac >= 3 and ac * 10 >= depth * 3:
            calls.append(Call(p, BASES[r], BASES[alt], genotype(ac, depth), depth, ac))
        while di < len(dels) and dels[di] // 16 < p:
            di += 1
        while di < len(dels) and dels[di] // 16 == p:
            key = dels[di]
            c = 0
            while di < len(dels) and dels[di] == key:
                c += 1
                di += 1
            ln = key % 16
            dp = sum(counts[p * 5:p * 5 + 5]) if p < L else 0
            if p + ln <= L and c >= 3 and c * 10 >= dp * 3:
                calls.append(Call(p, bases_text(ref, p - 1, p + ln), BASES[ref[p - 1]], genotype(c, dp), dp, c))
        while ii < len(inss) and inss[ii] // 65536 // 16 < p:
            ii += 1
        while ii < len(inss) and inss[ii] // 65536 // 16 == p:
            key = inss[ii]
            c = 0
            while ii < len(inss) and inss[ii] == key:
                c += 1
                ii += 1
            ln = key // 65536 % 16
            code = key % 65536
            s = ""
            for x in range(ln):
                s = BASES[code % 4] + s
                code //= 4
            dp = sum(counts[p * 5:p * 5 + 5]) if p < L else 0
            if c >= 3 and c * 10 >= dp * 3:
                anchor = BASES[ref[p - 1]]
                calls.append(Call(p, anchor, anchor + s, genotype(c, dp), dp, c))
    return calls


# ------------------------------------------------------------ reports

def cigar_text(a):
    return "".join(f"{a.lens[t]}{'MID'[a.ops[t]]}" for t in range(len(a.ops)))


def depth_at(ref, alns):
    depth = [0] * len(ref)
    for a in alns:
        if not a.mapped:
            continue
        j = a.pos
        for t in range(len(a.ops)):
            if a.ops[t] != 1:
                for _ in range(a.lens[t]):
                    depth[j] += 1
                    j += 1
    return depth


def coverage_report(name, ref, alns):
    depth = depth_at(ref, alns)
    bands = [0] * 6
    total, zero, deepest = 0, 0, 0
    for d in depth:
        total += d
        if d == 0:
            zero += 1
        deepest = max(deepest, d)
        b = 0 if d == 0 else min(5, 1 + d // 5)
        bands[b] += 1
    mean = total * 100 // len(ref)
    out = f"{name} coverage: mean {mean // 100}.{mean % 100:02}x, max {deepest}, {zero} bases uncovered\n"
    labels = ["0", "1-4", "5-9", "10-14", "15-19", "20+"]
    out += "  depth " + ", ".join(f"{labels[i]}: {bands[i]}" for i in range(6))
    return out


def mapping_report(name, reads, alns):
    mapped, rev, placed, nm, indel = 0, 0, 0, 0, 0
    q = [0] * 4
    for i in range(len(alns)):
        a = alns[i]
        if not a.mapped:
            continue
        mapped += 1
        if a.rev:
            rev += 1
        if a.mapq == 0:
            q[0] += 1
        elif a.mapq < 30:
            q[1] += 1
        elif a.mapq < 60:
            q[2] += 1
        else:
            q[3] += 1
        if a.mapq >= MIN_MAPQ and abs(a.pos - reads[i].truth) <= 5:
            placed += 1
        nm += a.nm
        if 1 in a.ops or 2 in a.ops:
            indel += 1
    out = f"{name} mapping: {len(alns)} reads, {mapped} mapped, {len(alns) - mapped} unmapped, {rev} reverse\n"
    out += f"  mapq 0: {q[0]}, 1-29: {q[1]}, 30-59: {q[2]}, 60: {q[3]}; {placed} placed at their origin; "
    out += f"{nm} edits, {indel} reads with gaps"
    return out


def variant_report(name, d, calls):
    truth = d.truth
    tp, fp = 0, 0
    lines = []
    for c in calls:
        t = c.text()
        if t in truth:
            tp += 1
            mark = "true"
        else:
            fp += 1
            mark = "FALSE"
        lines.append(f"  {name} {t} {c.gt} depth {c.depth} alt {c.ac} {mark}")
    called = [c.text() for c in calls]
    missed = [t for t in truth if t not in called]
    out = f"{name} variants: {len(calls)} called, {tp} true, {fp} false, {len(missed)} missed of {len(truth)}"
    for ln in lines:
        out += "\n" + ln
    for t in missed:
        out += f"\n  {name} {t} missed"
    return out


# The mapped read with the most edits, drawn against the reference.
def worst_report(name, ref, reads, alns):
    w = -1
    for i in range(len(alns)):
        if alns[i].mapped and (w < 0 or alns[i].nm > alns[w].nm):
            w = i
    if w < 0:
        return f"{name}: no mapped reads"
    a = alns[w]
    top, mid, bot = [], [], []
    j = a.pos
    q = 0
    for t in range(len(a.ops)):
        for _ in range(a.lens[t]):
            if a.ops[t] == 0:
                top.append(BASES[ref[j]])
                bot.append(BASES[a.seq[q]])
                mid.append("|" if ref[j] == a.seq[q] else ".")
                j += 1
                q += 1
            elif a.ops[t] == 1:
                top.append("-")
                bot.append(BASES[a.seq[q]])
                mid.append(" ")
                q += 1
            else:
                top.append(BASES[ref[j]])
                bot.append("-")
                mid.append(" ")
                j += 1
    strand = "-" if a.rev else "+"
    out = f"{name} most edited: {reads[w].name} at {a.pos + 1}{strand} {cigar_text(a)} edits {a.nm} score {a.score} mapq {a.mapq}\n"
    out += "  ref  " + "".join(top) + "\n"
    out += "       " + "".join(mid) + "\n"
    out += "  read " + "".join(bot)
    return out


def composition_report(name, ref, idx):
    gc = []
    i = 0
    while i < len(ref.seq):
        hi = min(i + 1000, len(ref.seq))
        c = 0
        for j in range(i, hi):
            if ref.seq[j] == 1 or ref.seq[j] == 2:
                c += 1
        gc.append(c * 100 // (hi - i))
        i = hi
    run, longest, at = 1, 1, 0
    for j in range(1, len(ref.seq)):
        if ref.seq[j] == ref.seq[j - 1]:
            run += 1
            if run > longest:
                longest, at = run, j - run + 1
        else:
            run = 1
    distinct, repeated = 0, 0
    h = 2166136261
    i = 0
    while i < len(idx):
        code = idx[i] // POS_KEY
        n = 0
        while i < len(idx) and idx[i] // POS_KEY == code:
            h = ((h ^ (idx[i] % POS_KEY)) * 16777619) & MASK32
            n += 1
            i += 1
        distinct += 1
        if n > 1:
            repeated += 1
    out = f"{name} composition: {len(ref.seq)} bases, element at {ref.elem_at + 1}, "
    out += f"copy of {ref.dup_from + 1}-{ref.dup_from + DUP_LEN} at {ref.dup_at + 1}\n"
    out += "  gc% per kb " + " ".join(str(g) for g in gc)
    out += f"; longest run {longest} {BASES[ref.seq[at]]} at {at + 1}\n"
    out += f"  {distinct} distinct {K}-mers, {repeated} repeated, index fnv {h:08x}"
    return out


# ------------------------------------------------------------ the cohort

class Cohort:
    def __init__(self):
        self.entries = []
        self.vcf = ""


def register(db, name, ref, calls):
    out = []
    fresh = 0
    for c in calls:
        lo = max(0, c.pos - 9)
        sig = bases_text(ref, lo, c.pos - 1) + ":" + c.ref + ">" + c.alt
        found = -1
        for i in range(len(db.entries)):
            if db.entries[i].sig == sig:
                found = i
        if found >= 0:
            e = db.entries[found]
            e.samples += 1
            out.append(f"{name} {c.text()} = kv{e.id} first on {e.chrom} {e.pos}, now in {e.samples} samples")
            idn = e.id
        else:
            idn = len(db.entries) + 1
            db.entries.append(Entry(sig, idn, name, c.pos))
            fresh += 1
        db.vcf += f"{name}\t{c.pos}\tkv{idn}\t{c.ref}\t{c.alt}\t{c.depth}\tGT\t{c.gt}\n"
    out.insert(0, f"{name} registered: {len(calls)} calls, {fresh} new, {len(calls) - fresh} seen before")
    return out


# ------------------------------------------------------------ the job

def run():
    element = random_bases(99, ELEM_LEN, 7)
    # Four chromosomes.
    r1 = build_ref("chr1", 11, element)
    r2 = build_ref("chr2", 23, element)
    r3 = build_ref("chr3", 37, element)
    r4 = build_ref("chr4", 41, element)

    # Four donors, one per chromosome.
    d1 = make_donor(r1, 11)
    d2 = make_donor(r2, 23)
    d3 = make_donor(r3, 37)
    d4 = make_donor(r4, 41)

    # Four sequencing runs.
    q1 = sequence("chr1", d1, 11)
    q2 = sequence("chr2", d2, 23)
    q3 = sequence("chr3", d3, 37)
    q4 = sequence("chr4", d4, 41)

    # Eight writes.
    w = [
        write_text("chr1.fa", encode_fasta(r1)),
        write_text("chr2.fa", encode_fasta(r2)),
        write_text("chr3.fa", encode_fasta(r3)),
        write_text("chr4.fa", encode_fasta(r4)),
        write_text("chr1.fq", encode_fastq(q1)),
        write_text("chr2.fq", encode_fastq(q2)),
        write_text("chr3.fq", encode_fastq(q3)),
        write_text("chr4.fq", encode_fastq(q4)),
    ]
    if not all(w):
        raise Bad("could not write the inputs")

    whole = read_text("chr3.fq")
    if not write_text("broken.fq", damage(whole, 16)):
        raise Bad("could not write broken.fq")

    # Eight reads.
    t1 = read_text("chr1.fa")
    t2 = read_text("chr2.fa")
    t3 = read_text("chr3.fa")
    t4 = read_text("chr4.fa")
    u1 = read_text("chr1.fq")
    u2 = read_text("chr2.fq")
    u3 = read_text("chr3.fq")
    u4 = read_text("chr4.fq")
    for path, t in [("chr1.fa", t1), ("chr2.fa", t2), ("chr3.fa", t3), ("chr4.fa", t4),
                    ("chr1.fq", u1), ("chr2.fq", u2), ("chr3.fq", u3), ("chr4.fq", u4)]:
        print(f"{path}: {len(t)} bytes, fnv {text_sum(t):08x}")

    # Eight parses.
    g1 = parse_fasta("chr1", t1)
    g2 = parse_fasta("chr2", t2)
    g3 = parse_fasta("chr3", t3)
    g4 = parse_fasta("chr4", t4)
    p1 = parse_fastq("chr1", u1)
    p2 = parse_fastq("chr2", u2)
    p3 = parse_fastq("chr3", u3)
    p4 = parse_fastq("chr4", u4)
    try:
        parse_fastq("chr3", read_text("broken.fq"))
        print("broken.fq: parsed")
    except Bad as e:
        print(e)
    try:
        parse_fasta("chr4", t2)
        print("chr2.fa as chr4: parsed")
    except Bad as e:
        print(e)
    same = g1 == r1.seq and g2 == r2.seq and g3 == r3.seq and g4 == r4.seq
    print(f"parsed references match the built ones: {'true' if same else 'false'}")
    same = same_reads(p1, q1) and same_reads(p2, q2) and same_reads(p3, q3) and same_reads(p4, q4)
    print(f"parsed reads match the sequenced ones: {'true' if same else 'false'}")

    # Four k-mer indexes.
    x1 = build_index(g1)
    x2 = build_index(g2)
    x3 = build_index(g3)
    x4 = build_index(g4)

    # Four alignment runs (the hot group).
    a1 = align_all(g1, x1, p1)
    a2 = align_all(g2, x2, p2)
    a3 = align_all(g3, x3, p3)
    a4 = align_all(g4, x4, p4)

    # Four variant calls.
    c1 = call_variants(g1, a1)
    c2 = call_variants(g2, a2)
    c3 = call_variants(g3, a3)
    c4 = call_variants(g4, a4)

    # Reports that only read what came before.
    reports = [
        coverage_report("chr1", g1, a1),
        coverage_report("chr2", g2, a2),
        coverage_report("chr3", g3, a3),
        coverage_report("chr4", g4, a4),
        mapping_report("chr1", p1, a1),
        mapping_report("chr2", p2, a2),
        mapping_report("chr3", p3, a3),
        mapping_report("chr4", p4, a4),
        variant_report("chr1", d1, c1),
        variant_report("chr2", d2, c2),
        variant_report("chr3", d3, c3),
        variant_report("chr4", d4, c4),
        worst_report("chr1", g1, p1, a1),
        worst_report("chr2", g2, p2, a2),
        worst_report("chr3", g3, p3, a3),
        worst_report("chr4", g4, p4, a4),
        composition_report("chr1", r1, x1),
        composition_report("chr2", r2, x2),
        composition_report("chr3", r3, x3),
        composition_report("chr4", r4, x4),
    ]
    for line in reports:
        print(line)

    # One cohort database: ids go to calls in the order they arrive, and a
    # call seen before points at the chromosome that brought it first.
    db = Cohort()
    for line in register(db, "chr1", g1, c1):
        print(line)
    for line in register(db, "chr2", g2, c2):
        print(line)
    for line in register(db, "chr3", g3, c3):
        print(line)
    for line in register(db, "chr4", g4, c4):
        print(line)
    if not write_text("cohort.vcf", db.vcf):
        raise Bad("could not write cohort.vcf")
    back = read_text("cohort.vcf")
    rows = len(back.split("\n")) - 1
    print(f"cohort.vcf: {rows} rows, {len(db.entries)} variants, {len(back)} bytes, fnv {text_sum(back):08x}")

    for p in ["chr1.fa", "chr2.fa", "chr3.fa", "chr4.fa", "chr1.fq", "chr2.fq", "chr3.fq", "chr4.fq",
              "broken.fq", "cohort.vcf"]:
        try:
            os.remove(p)
        except OSError:
            pass


def main():
    try:
        run()
    except Bad as e:
        print(f"read_align failed: {e}")


main()
