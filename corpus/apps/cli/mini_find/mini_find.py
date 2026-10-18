# mini_find in Python: the same program as mini_find.kara, written the
# same way. Its output is expected.out. real.py checks it against GNU
# find 4.9.
import os, shutil, sys, stat
import mk

NOW_MS = 1792065600000  # 2026-10-15 12:00:00 UTC
DAY_MS = 86400000
SHIFT_MS = 0  # real.py moves the tree to its own clock

# ------------------------------------------------------------ text


def shell_words(line):
    words, cur, have, i = [], "", False, 0
    while i < len(line):
        c = line[i]
        if c == "'":
            j = line.index("'", i + 1)
            cur += line[i + 1:j]
            have, i = True, j + 1
        elif c == '"':
            i += 1
            while line[i] != '"':
                if line[i] == "\\" and line[i + 1] in '"\\$`':
                    i += 1
                cur += line[i]
                i += 1
            have, i = True, i + 1
        elif c == "\\":
            cur += line[i + 1]
            have, i = True, i + 2
        elif c == " ":
            if have:
                words.append(cur)
            cur, have, i = "", False, i + 1
        else:
            cur += c
            have, i = True, i + 1
    if have:
        words.append(cur)
    return words


def is_int(s):
    return len(s) > 0 and all("0" <= c <= "9" for c in s)


def lower(s):
    return "".join(chr(ord(c) + 32) if "A" <= c <= "Z" else c for c in s)


# ------------------------------------------------------------ dates

MONTHS = "Jan Feb Mar Apr May Jun Jul Aug Sep Oct Nov Dec".split()
DAYS = "Thu Fri Sat Sun Mon Tue Wed".split()  # day 0 was a Thursday


def days_from_civil(y, m, d):
    if m <= 2:
        y -= 1
    era = (y if y >= 0 else y - 399) // 400
    yoe = y - era * 400
    mp = m - 3 if m > 2 else m + 9
    doy = (153 * mp + 2) // 5 + d - 1
    doe = yoe * 365 + yoe // 4 - yoe // 100 + doy
    return era * 146097 + doe - 719468


def civil_from_days(z):
    z += 719468
    era = (z if z >= 0 else z - 146096) // 146097
    doe = z - era * 146097
    yoe = (doe - doe // 1460 + doe // 36524 - doe // 146096) // 365
    y = yoe + era * 400
    doy = doe - (365 * yoe + yoe // 4 - yoe // 100)
    mp = (5 * doy + 2) // 153
    d = doy - (153 * mp + 2) // 5 + 1
    m = mp + 3 if mp < 10 else mp - 9
    if m <= 2:
        y += 1
    return y, m, d


def two(n):
    return "%02d" % n


# "2026-10-14", "2026-10-14 08:00" or "2026-10-14T08:00:00", in UTC.
def parse_stamp(s):
    if len(s) < 10 or s[4] != "-" or s[7] != "-":
        return None
    y, mo, d = s[0:4], s[5:7], s[8:10]
    if not (is_int(y) and is_int(mo) and is_int(d)):
        return None
    hh, mm, ss = "0", "0", "0"
    rest = s[10:]
    if rest != "":
        if rest[0] not in " T" or len(rest) not in (6, 9) or rest[3] != ":":
            return None
        hh, mm = rest[1:3], rest[4:6]
        if len(rest) == 9:
            if rest[6] != ":":
                return None
            ss = rest[7:9]
        if not (is_int(hh) and is_int(mm) and is_int(ss)):
            return None
    y, mo, d, hh, mm, ss = int(y), int(mo), int(d), int(hh), int(mm), int(ss)
    if mo < 1 or mo > 12 or d < 1 or d > 31 or hh > 23 or mm > 59 or ss > 59:
        return None
    return ((days_from_civil(y, mo, d) * 24 + hh) * 60 + mm) * 60000 + ss * 1000


def time_part(ms, c):
    secs = ms // 1000
    frac = "%03d0000000" % (ms % 1000)
    days = secs // 86400
    sod = secs % 86400
    y, m, d = civil_from_days(days)
    h, mi, s = sod // 3600, sod % 3600 // 60, sod % 60
    if c == "Y":
        return str(y)
    if c == "m":
        return two(m)
    if c == "d":
        return two(d)
    if c == "H":
        return two(h)
    if c == "M":
        return two(mi)
    if c == "S":
        return two(s) + "." + frac
    if c == "T":
        return two(h) + ":" + two(mi) + ":" + two(s) + "." + frac
    if c == "@":
        return str(secs) + "." + frac
    if c == "j":
        return "%03d" % (days - days_from_civil(y, 1, 1) + 1)
    if c == "a":
        return DAYS[days % 7]
    if c == "b":
        return MONTHS[m - 1]
    if c == "D":
        return two(m) + "/" + two(d) + "/" + two(y % 100)
    if c == "F":
        return str(y) + "-" + two(m) + "-" + two(d)
    if c == "k":
        return "%2d" % h
    if c == "r":
        h12 = 12 if h % 12 == 0 else h % 12
        return two(h12) + ":" + two(mi) + ":" + two(s) + (" AM" if h < 12 else " PM")
    return None


def ctime(ms):
    secs = ms // 1000
    days = secs // 86400
    y, m, d = civil_from_days(days)
    return "%s %s %2d %s %d" % (DAYS[days % 7], MONTHS[m - 1], d, time_part(ms, "T"), y)


# ------------------------------------------------------------ the tree


def content(path, size):
    out = []
    for i in range(size):
        out.append("\n" if i % 40 == 39 else chr(97 + (i * 7 + len(path)) % 26))
    return "".join(out)


def build_tree():
    root = "data/proj"
    if os.path.lexists(root):
        shutil.rmtree(root)
    entries = [l.split(" ") for l in mk.FILES["manifest.txt"].split("\n") if l != ""]
    stamps = []
    for e in entries:
        if e[0] == "d":
            path = root if e[3] == "." else root + "/" + " ".join(e[3:])
            os.mkdir(path)
            stamps.append((path, int(e[1], 8), parse_stamp(e[2])))
        elif e[0] == "f":
            path = root + "/" + " ".join(e[4:])
            open(path, "w").write(content(path, int(e[3])))
            stamps.append((path, int(e[1], 8), parse_stamp(e[2])))
        else:
            os.symlink(e[1], root + "/" + e[2])
    # Deepest first, so a directory's time is set after its entries.
    for path, mode, ms in reversed(stamps):
        ms += SHIFT_MS
        os.utime(path, ns=(ms * 1000000, ms * 1000000))
        os.chmod(path, mode)


# ------------------------------------------------------------ status


class Info:
    def __init__(self, kind, size, mtime, mode):
        self.kind, self.size, self.mtime, self.mode = kind, size, mtime, mode


def info(path, follow):
    try:
        st = os.stat(path) if follow else os.lstat(path)
    except OSError:
        return None
    if stat.S_ISLNK(st.st_mode):
        kind = "l"
    elif stat.S_ISDIR(st.st_mode):
        kind = "d"
    else:
        kind = "f"
    return Info(kind, st.st_size, st.st_mtime_ns // 1000000, st.st_mode & 0o7777)


def read_link(path):
    try:
        return os.readlink(path)
    except OSError:
        return ""


# ------------------------------------------------------------ patterns


# fnmatch as find uses it: no special leading dot, `*` matches `/`.
def wild(p, i, s, j):
    while i < len(p):
        c = p[i]
        if c == "*":
            while i < len(p) and p[i] == "*":
                i += 1
            if i == len(p):
                return True
            for k in range(j, len(s) + 1):
                if wild(p, i, s, k):
                    return True
            return False
        if j == len(s):
            return False
        if c == "?":
            i, j = i + 1, j + 1
        elif c == "[":
            ok, ni = bracket(p, i, s[j])
            if ni < 0:
                if s[j] != "[":
                    return False
                i, j = i + 1, j + 1
            elif not ok:
                return False
            else:
                i, j = ni, j + 1
        else:
            if c == "\\" and i + 1 < len(p):
                i += 1
                c = p[i]
            if s[j] != c:
                return False
            i, j = i + 1, j + 1
    return j == len(s)


# Matches one character against the class at p[i]; returns the index
# after the class, or -1 if it is not closed.
def bracket(p, i, ch):
    k = i + 1
    neg = False
    if k < len(p) and p[k] in "!^":
        neg = True
        k += 1
    found = False
    first = True
    while k < len(p) and (first or p[k] != "]"):
        first = False
        lo = p[k]
        if lo == "\\" and k + 1 < len(p):
            k += 1
            lo = p[k]
        if k + 2 < len(p) and p[k + 1] == "-" and p[k + 2] != "]":
            hi = p[k + 2]
            if lo <= ch <= hi:
                found = True
            k += 3
        else:
            if ch == lo:
                found = True
            k += 1
    if k >= len(p):
        return False, -1
    return found != neg, k + 1


def matches(pat, s, fold):
    if fold:
        return wild(lower(pat), 0, lower(s), 0)
    return wild(pat, 0, s, 0)


# ------------------------------------------------------------ modes

WHO = {"u": 0o4700, "g": 0o2070, "o": 0o1007, "a": 0o7777}
PERM = {"r": 0o444, "w": 0o222, "x": 0o111, "s": 0o6000, "t": 0o1000}


def parse_mode(s):
    if s != "" and all("0" <= c <= "7" for c in s):
        v = int(s, 8)
        return v if v <= 0o7777 else None
    mode = 0
    for clause in s.split(","):
        i, who = 0, 0
        while i < len(clause) and clause[i] in WHO:
            who |= WHO[clause[i]]
            i += 1
        if who == 0:
            who = 0o7777
        if i == len(clause):
            return None
        while i < len(clause):
            op = clause[i]
            if op not in "+-=":
                return None
            i += 1
            bits = 0
            while i < len(clause) and clause[i] in PERM:
                bits |= PERM[clause[i]]
                i += 1
            bits &= who
            if op == "+":
                mode |= bits
            elif op == "-":
                mode &= ~bits
            else:
                mode = (mode & ~who) | bits
    return mode


def mode_string(kind, mode):
    out = {"d": "d", "l": "l"}.get(kind, "-")
    for shift, special, ch in ((6, 0o4000, "s"), (3, 0o2000, "s"), (0, 0o1000, "t")):
        bits = mode >> shift
        out += "r" if bits & 4 else "-"
        out += "w" if bits & 2 else "-"
        x = bits & 1
        if mode & special:
            out += ch if x else ch.upper()
        else:
            out += "x" if x else "-"
    return out


# ------------------------------------------------------------ expressions

# Node kinds.
AND, OR, NOT, COMMA, TRUE, FALSE = "and", "or", "not", "comma", "true", "false"


class Node:
    def __init__(self, kind, a=-1, b=-1, s="", n=0, cmp=0, fold=False):
        self.kind, self.a, self.b, self.s, self.n, self.cmp, self.fold = kind, a, b, s, n, cmp, fold


class Fail(Exception):
    pass


class Find:
    def __init__(self, job):
        self.job = job
        self.nodes = []
        self.follow = "P"
        self.maxdepth = -1
        self.mindepth = 0
        self.depth_first = False
        self.daystart = False
        self.has_action = False
        self.has_prune = False
        self.has_delete = False
        self.depth_given = False
        self.quit = False
        self.args = []
        self.pos = 0

    def add(self, node):
        self.nodes.append(node)
        return len(self.nodes) - 1

    def fail(self, msg):
        self.job.err += "find: " + msg + "\n"
        raise Fail()

    def peek(self):
        return self.args[self.pos] if self.pos < len(self.args) else None

    def need(self, name):
        if self.pos >= len(self.args):
            self.fail("missing argument to `%s'" % name)
        a = self.args[self.pos]
        self.pos += 1
        return a

    # comma < or < and < not < primary
    def parse_comma(self):
        left = self.parse_or()
        while self.peek() == ",":
            self.pos += 1
            if self.peek() is None:
                self.fail("expected an expression after ','")
            left = self.add(Node(COMMA, left, self.parse_or()))
        return left

    def parse_or(self):
        left = self.parse_and()
        while self.peek() in ("-o", "-or"):
            op = self.args[self.pos]
            self.pos += 1
            if self.peek() is None or self.peek() in (")", ","):
                self.fail("expected an expression after '%s'" % op)
            left = self.add(Node(OR, left, self.parse_and()))
        return left

    def parse_and(self):
        left = self.parse_not()
        while True:
            p = self.peek()
            if p is None or p in (")", ",", "-o", "-or"):
                return left
            if p in ("-a", "-and"):
                self.pos += 1
                if self.peek() is None or self.peek() in (")", ","):
                    self.fail("expected an expression after '%s'" % p)
            left = self.add(Node(AND, left, self.parse_not()))

    def parse_not(self):
        p = self.peek()
        if p in ("!", "-not"):
            self.pos += 1
            if self.peek() is None or self.peek() in (")", ","):
                self.fail("expected an expression after '%s'" % p)
            return self.add(Node(NOT, self.parse_not()))
        return self.parse_primary()

    def parse_primary(self):
        a = self.args[self.pos]
        self.pos += 1
        if a == "(":
            if self.peek() == ")":
                self.fail("invalid expression; empty parentheses are not allowed.")
            if self.peek() is None:
                self.fail("invalid expression; expected to find a ')' but didn't see one. "
                          "Perhaps you need an extra predicate after '('")
            e = self.parse_comma()
            if self.peek() != ")":
                self.fail("invalid expression; expected to find a ')' but didn't see one. "
                          "Perhaps you need an extra predicate after '('")
            self.pos += 1
            return e
        if a in ("-o", "-or", "-a", "-and", ","):
            self.fail("invalid expression; you have used a binary operator '%s' with nothing before it." % a)
        if a == ")":
            self.fail("you have too many ')'")
        if not a.startswith("-") or a == "-":
            self.fail("paths must precede expression: `%s'" % a)
        name = a
        if name in ("-name", "-iname", "-path", "-ipath", "-wholename", "-iwholename", "-lname", "-ilname"):
            kind = {"-iname": "-name", "-ipath": "-path", "-wholename": "-path",
                    "-iwholename": "-path", "-ilname": "-lname"}.get(name, name)
            return self.add(Node(kind, s=self.need(name), fold=name.startswith("-i")))
        if name in ("-type", "-xtype"):
            arg = self.need(name)
            types = ""
            for i, c in enumerate(arg):
                if i % 2 == 1:
                    if c != ",":
                        self.fail("Must separate multiple arguments to %s using: ','" % name)
                    if i == len(arg) - 1:
                        self.fail("Last file type in list argument to %s is missing, "
                                  "i.e., list is ending on: ','" % name)
                elif c in "fdl":
                    types += c
                else:
                    self.fail("Unknown argument to %s: %s" % (name, c))
            return self.add(Node(name, s=types))
        if name == "-size":
            arg = self.need(name)
            cmp, num = 0, arg
            if num[:1] in ("+", "-"):
                cmp, num = (1 if num[0] == "+" else -1), num[1:]
            unit = 512
            if num != "" and not ("0" <= num[-1] <= "9"):
                u = num[-1]
                units = {"c": 1, "w": 2, "b": 512, "k": 1024, "M": 1048576, "G": 1073741824}
                if u not in units:
                    self.fail("invalid -size type `%s'" % u)
                unit, num = units[u], num[:-1]
            if not is_int(num):
                self.fail("Invalid argument `%s' to -size" % arg)
            return self.add(Node(name, n=int(num), cmp=cmp, b=unit))
        if name in ("-mtime", "-mmin"):
            arg = self.need(name)
            cmp, num = 0, arg
            if num[:1] in ("+", "-"):
                cmp, num = (1 if num[0] == "+" else -1), num[1:]
            if not is_int(num):
                self.fail("invalid argument `%s' to `%s'" % (arg, name))
            return self.add(Node(name, n=int(num), cmp=cmp))
        if name == "-perm":
            arg = self.need(name)
            cmp, m = 0, arg
            if m[:1] in ("-", "/"):
                cmp, m = (-1 if m[0] == "-" else 1), m[1:]
            mode = parse_mode(m)
            if mode is None:
                self.fail("invalid mode '%s'" % arg)
            return self.add(Node(name, n=mode, cmp=cmp))
        if name == "-newer":
            arg = self.need(name)
            i = info(arg, self.follow != "P")
            if i is None:
                self.fail("'%s': No such file or directory" % arg)
            return self.add(Node("-newermt", n=i.mtime))
        if name == "-newermt":
            arg = self.need(name)
            ms = parse_stamp(arg)
            if ms is None:
                self.fail("I cannot figure out how to interpret '%s' as a date or time" % arg)
            return self.add(Node(name, n=ms))
        if name in ("-maxdepth", "-mindepth"):
            arg = self.need(name)
            if not is_int(arg):
                self.fail("Expected a positive decimal integer argument to %s, but got '%s'" % (name, arg))
            if name == "-maxdepth":
                self.maxdepth = int(arg)
            else:
                self.mindepth = int(arg)
            return self.add(Node(TRUE))
        if name in ("-depth", "-d"):
            self.depth_first = True
            self.depth_given = True
            return self.add(Node(TRUE))
        if name == "-daystart":
            self.daystart = True
            return self.add(Node(TRUE))
        if name == "-true":
            return self.add(Node(TRUE))
        if name == "-false":
            return self.add(Node(FALSE))
        if name == "-empty":
            return self.add(Node(name))
        if name == "-print":
            self.has_action = True
            return self.add(Node("-printf", s="%p\n"))
        if name == "-printf":
            self.has_action = True
            return self.add(Node(name, s=self.check_format(self.need(name))))
        if name == "-prune":
            self.has_prune = True
            return self.add(Node(name))
        if name == "-quit":
            return self.add(Node(name))
        if name == "-delete":
            self.has_action = True
            self.has_delete = True
            self.depth_first = True
            return self.add(Node(name))
        self.fail("unknown predicate `%s'" % name)

    # Warns once about each unknown escape or directive, as find does
    # while it parses the format.
    def check_format(self, f):
        i = 0
        while i < len(f):
            c = f[i]
            if c == "\\" and i + 1 < len(f):
                e = f[i + 1]
                if e not in "\\abfnrtvc" and not ("0" <= e <= "7"):
                    self.job.err += "find: warning: unrecognized escape `\\%s'\n" % e
                i += 2
            elif c == "%":
                i += 1
                while i < len(f) and f[i] in "-0123456789.":
                    i += 1
                if i >= len(f):
                    self.fail("error: % at end of format string")
                d = f[i]
                if d == "T":
                    i += 1
                elif d not in "pPfhdyYlmMst%":
                    self.job.err += "find: warning: unrecognized format directive `%%%s'\n" % d
                i += 1
            else:
                i += 1
        return f


# ------------------------------------------------------------ evaluation


class Entry:
    def __init__(self, path, root, depth, info, link_info):
        self.path, self.root, self.depth = path, root, depth
        self.info = info            # as find sees it (lstat or stat)
        self.link_info = link_info  # lstat


def basename(e):
    p = e.path
    s = p.rstrip("/") or "/"
    i = s.rfind("/")
    f = s[i + 1:] if i >= 0 else s
    if s != p and f != "/":
        f += "/"
    return f, s


def dirname(e):
    s = e.path.rstrip("/") or "/"
    i = s.rfind("/")
    if i < 0:
        return "."
    return s[:i] if i > 0 else "/"


def name_of(e):
    f, s = basename(e)
    return f.rstrip("/") or "/"


def other_type(f, e):
    # -xtype: stat when find uses lstat, and lstat when it uses stat.
    if f.follow == "L" or (f.follow == "H" and e.depth == 0):
        return e.link_info.kind
    if e.link_info.kind != "l":
        return e.link_info.kind
    t = info(e.path, True)
    return "l" if t is None else t.kind


def is_empty(e):
    if e.info.kind == "f":
        return e.info.size == 0
    if e.info.kind == "d":
        try:
            return len(os.listdir(e.path)) == 0
        except OSError:
            return False
    return False


def compare(cmp, have, want):
    if cmp > 0:
        return have > want
    if cmp < 0:
        return have < want
    return have == want


def eval_node(f, k, e):
    n = f.nodes[k]
    kind = n.kind
    if kind == AND:
        return eval_node(f, n.a, e) and not f.quit and eval_node(f, n.b, e)
    if kind == OR:
        return eval_node(f, n.a, e) or (not f.quit and eval_node(f, n.b, e))
    if kind == COMMA:
        eval_node(f, n.a, e)
        return not f.quit and eval_node(f, n.b, e)
    if kind == NOT:
        return not eval_node(f, n.a, e)
    if kind == TRUE:
        return True
    if kind == FALSE:
        return False
    if kind == "-name":
        return matches(n.s, name_of(e), n.fold)
    if kind == "-path":
        return matches(n.s, e.path, n.fold)
    if kind == "-lname":
        return e.link_info.kind == "l" and matches(n.s, read_link(e.path), n.fold)
    if kind == "-type":
        return e.info.kind in n.s
    if kind == "-xtype":
        return other_type(f, e) in n.s
    if kind == "-size":
        if n.b == 1:
            units = e.info.size
        else:
            units = (e.info.size + n.b - 1) // n.b
        return compare(n.cmp, units, n.n)
    if kind == "-mtime":
        now = NOW_MS
        if f.daystart:
            now = (NOW_MS // DAY_MS + 1) * DAY_MS
        # Whole days, rounded down.
        return compare(n.cmp, (now - e.info.mtime) // DAY_MS, n.n)
    if kind == "-mmin":
        # Minutes are not rounded: +30 is more than 30 minutes, 30 is
        # more than 29 and at most 30.
        age = NOW_MS - e.info.mtime
        if n.cmp > 0:
            return age > n.n * 60000
        if n.cmp < 0:
            return age < n.n * 60000
        return (n.n - 1) * 60000 < age <= n.n * 60000
    if kind == "-perm":
        m = e.info.mode
        if n.cmp == 0:
            return m == n.n
        if n.cmp < 0:
            return m & n.n == n.n
        return n.n == 0 or m & n.n != 0
    if kind == "-newermt":
        return e.info.mtime > n.n
    if kind == "-empty":
        return is_empty(e)
    if kind == "-printf":
        f.job.out += render(n.s, e, f)
        return True
    if kind == "-prune":
        if not f.depth_first:
            e.prune = True
        return True
    if kind == "-quit":
        f.quit = True
        return True
    if kind == "-delete":
        return delete(f, e)
    raise AssertionError(kind)


def delete(f, e):
    if e.link_info.kind == "d":
        if len(os.listdir(e.path)) > 0:
            f.job.err += "find: cannot delete '%s': Directory not empty\n" % e.path
            f.job.status = 1
            return False
        os.rmdir(e.path)
    else:
        os.remove(e.path)
    return True


def pad(s, width, left, prec):
    if prec >= 0:
        s = s[:prec]
    if len(s) >= width:
        return s
    return s + " " * (width - len(s)) if left else " " * (width - len(s)) + s


ESCAPES = {"\\": "\\", "a": "\a", "b": "\b", "f": "\f", "n": "\n", "r": "\r", "t": "\t", "v": "\v"}


def render(fmt, e, f):
    out, i = "", 0
    while i < len(fmt):
        c = fmt[i]
        if c == "\\" and i + 1 < len(fmt):
            x = fmt[i + 1]
            if x == "c":
                return out
            if "0" <= x <= "7":
                j, v = i + 1, 0
                while j < len(fmt) and j < i + 4 and "0" <= fmt[j] <= "7":
                    v = v * 8 + ord(fmt[j]) - 48
                    j += 1
                out += chr(v)
                i = j
                continue
            out += ESCAPES.get(x, "\\" + x)
            i += 2
            continue
        if c != "%":
            out += c
            i += 1
            continue
        i += 1
        left = False
        if fmt[i] == "-":
            left = True
            i += 1
        width = 0
        while "0" <= fmt[i] <= "9":
            width = width * 10 + ord(fmt[i]) - 48
            i += 1
        prec = -1
        if fmt[i] == ".":
            i += 1
            prec = 0
            while "0" <= fmt[i] <= "9":
                prec = prec * 10 + ord(fmt[i]) - 48
                i += 1
        d = fmt[i]
        i += 1
        if d == "%":
            out += "%"
            continue
        if d == "T":
            t = time_part(e.info.mtime, fmt[i])
            if t is None:
                t = "?"
            i += 1
            out += pad(t, width, left, prec)
            continue
        v = directive(d, e, f)
        if v is None:
            out += "%" + d
        else:
            out += pad(v, width, left, prec)
    return out


def directive(d, e, f):
    if d == "p":
        return e.path
    if d == "P":
        r = e.root
        p = e.path
        if p == r:
            return ""
        return p[len(r) + 1:] if not r.endswith("/") else p[len(r):]
    if d == "f":
        return basename(e)[0]
    if d == "h":
        return dirname(e)
    if d == "d":
        return str(e.depth)
    if d == "y":
        return e.info.kind
    if d == "Y":
        if e.link_info.kind != "l":
            return e.link_info.kind
        t = info(e.path, True)
        return "N" if t is None else t.kind
    if d == "l":
        return read_link(e.path) if e.link_info.kind == "l" else ""
    if d == "m":
        return "%o" % e.info.mode
    if d == "M":
        return mode_string(e.info.kind, e.info.mode)
    if d == "s":
        return str(e.info.size)
    if d == "t":
        return ctime(e.info.mtime)
    return None


# ------------------------------------------------------------ the walk


def visit(f, top, path, root, depth):
    follow = f.follow == "L" or (f.follow == "H" and depth == 0)
    li = info(path, False)
    if li is None:
        f.job.err += "find: '%s': No such file or directory\n" % path
        f.job.status = 1
        return
    i = li
    if follow and li.kind == "l":
        t = info(path, True)
        if t is not None:
            i = t
    e = Entry(path, root, depth, i, li)
    e.prune = False
    evaluate = depth >= f.mindepth
    if evaluate and not f.depth_first:
        eval_node(f, top, e)
        if f.quit:
            return
    if i.kind == "d" and not e.prune and (f.maxdepth < 0 or depth < f.maxdepth):
        for name in sorted(os.listdir(path)):
            child = path + name if path.endswith("/") else path + "/" + name
            visit(f, top, child, root, depth + 1)
            if f.quit:
                return
    if evaluate and f.depth_first:
        eval_node(f, top, e)


class Job:
    def __init__(self):
        self.out, self.err, self.status = "", "", 0


def find(job, args):
    f = Find(job)
    k = 0
    while k < len(args) and args[k] in ("-H", "-L", "-P"):
        f.follow = args[k][1]
        k += 1
    paths = []
    while k < len(args) and not (args[k].startswith("-") and args[k] != "-") and args[k] not in ("(", "!", ","):
        paths.append(args[k])
        k += 1
    f.args = args[k:]
    try:
        top = -1
        if f.args:
            top = f.parse_comma()
            if f.pos < len(f.args):
                if f.args[f.pos] == ")":
                    f.fail("you have too many ')'")
                f.fail("paths must precede expression: `%s'" % f.args[f.pos])
        if f.has_delete and f.has_prune and not f.depth_given:
            f.fail("The -delete action automatically turns on -depth, but -prune does nothing when "
                   "-depth is in effect.  If you want to carry on anyway, just explicitly use the "
                   "-depth option.")
    except Fail:
        job.status = 1
        return
    if not f.has_action:
        pr = f.add(Node("-printf", s="%p\n"))
        top = pr if top < 0 else f.add(Node(AND, top, pr))
    if not paths:
        paths = ["."]
    for p in paths:
        visit(f, top, p, p, 0)
        if f.quit:
            break


def main():
    mk.write_all()
    build_tree()
    for line in mk.FILES["jobs.txt"].split("\n"):
        if line == "" or line.startswith("#"):
            continue
        job = Job()
        w = shell_words(line)
        if w[0] == "find":
            find(job, w[1:])
        elif w[0] == "reset":
            build_tree()
        out = "$ " + line + "\n" + job.out + job.err
        if job.status != 0:
            out += "[exit status %d]\n" % job.status
        print(out)


if __name__ == "__main__":
    main()
