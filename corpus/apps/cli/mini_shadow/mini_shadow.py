# mini_shadow in Python: the same program as mini_shadow.kara, written
# the same way. Its output is expected.out. real.py checks it against
# shadow 4.13's pwck, chage and passwd, OpenSSL 3.0's `passwd` and
# libxcrypt's crypt().
import base64, hashlib, os, shutil, sys
import mk, usage

TODAY = 20741  # 2026-10-15, in days since 1970-01-01

# ------------------------------------------------------------ text


def shell_words(line):
    words, cur, have, i = [], "", False, 0
    while i < len(line):
        c = line[i]
        if c == "'":
            j = line.index("'", i + 1)
            cur += line[i + 1:j]
            have, i = True, j + 1
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


# A number as getlong reads it: an optional minus and digits.
def parse_long(s):
    if s.startswith("-") and is_int(s[1:]):
        return -int(s[1:])
    if is_int(s):
        return int(s)
    return None


# ------------------------------------------------------------ dates

MONTHS = "Jan Feb Mar Apr May Jun Jul Aug Sep Oct Nov Dec".split()


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


def iso_date(days):
    y, m, d = civil_from_days(days)
    return "%04d-%02d-%02d" % (y, m, d)


def long_date(days):
    y, m, d = civil_from_days(days)
    return "%s %02d, %d" % (MONTHS[m - 1], d, y)


# A date as chage's strtoday reads it: a day number, or YYYY-MM-DD,
# which mktime normalizes (2026-02-30 is March 2).
def str_to_day(s):
    n = parse_long(s)
    if n is not None:
        return n
    p = s.split("-")
    if len(p) != 3 or not all(is_int(x) for x in p):
        return None
    y, m, d = int(p[0]), int(p[1]), int(p[2])
    y += (m - 1) // 12
    m = (m - 1) % 12 + 1
    return days_from_civil(y, m, 1) + d - 1


# ------------------------------------------------------------ crypt

ITOA64 = "./0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz"


def b64_24(b2, b1, b0, n):
    w = (b2 << 16) | (b1 << 8) | b0
    out = ""
    for _ in range(n):
        out += ITOA64[w & 0x3F]
        w >>= 6
    return out


def md5(b):
    return hashlib.md5(b).digest()


# The FreeBSD MD5 crypt, for $1$ and Apache's $apr1$.
def md5_crypt(pw, salt, magic):
    pw, salt = pw.encode(), salt.encode()[:8]
    final = md5(pw + salt + pw)
    ctx = pw + magic.encode() + salt
    i = len(pw)
    while i > 0:
        ctx += final[:min(16, i)]
        i -= 16
    i = len(pw)
    while i:
        ctx += b"\0" if i & 1 else pw[:1]
        i >>= 1
    final = md5(ctx)
    for i in range(1000):
        c = pw if i & 1 else final
        if i % 3:
            c += salt
        if i % 7:
            c += pw
        c += final if i & 1 else pw
        final = md5(c)
    f = final
    out = ""
    for a, b, c in ((0, 6, 12), (1, 7, 13), (2, 8, 14), (3, 9, 15), (4, 10, 5)):
        out += b64_24(f[a], f[b], f[c], 4)
    out += b64_24(0, 0, f[11], 2)
    return magic + salt.decode() + "$" + out


ORDER256 = [(0, 10, 20), (21, 1, 11), (12, 22, 2), (3, 13, 23), (24, 4, 14),
            (15, 25, 5), (6, 16, 26), (27, 7, 17), (18, 28, 8), (9, 19, 29)]
ORDER512 = [(0, 21, 42), (22, 43, 1), (44, 2, 23), (3, 24, 45), (25, 46, 4),
            (47, 5, 26), (6, 27, 48), (28, 49, 7), (50, 8, 29), (9, 30, 51),
            (31, 52, 10), (53, 11, 32), (12, 33, 54), (34, 55, 13), (56, 14, 35),
            (15, 36, 57), (37, 58, 16), (59, 17, 38), (18, 39, 60), (40, 61, 19),
            (62, 20, 41)]


def sha(kind, b):
    return hashlib.sha256(b).digest() if kind == 5 else hashlib.sha512(b).digest()


def repeat_to(d, n):
    out = b""
    while len(out) + len(d) <= n:
        out += d
    return out + d[:n - len(out)]


# Ulrich Drepper's SHA-crypt, for $5$ and $6$.
def sha_crypt(kind, pw, salt, rounds, show_rounds):
    pw, salt = pw.encode(), salt.encode()[:16]
    hl = 32 if kind == 5 else 64
    b = sha(kind, pw + salt + pw)
    a = pw + salt + repeat_to(b, len(pw))
    i = len(pw)
    while i > 0:
        a += b if i & 1 else pw
        i >>= 1
    a = sha(kind, a)
    p = repeat_to(sha(kind, pw * len(pw)), len(pw))
    s = repeat_to(sha(kind, salt * (16 + a[0])), len(salt))
    c = a
    for i in range(rounds):
        x = p if i & 1 else c
        if i % 3:
            x += s
        if i % 7:
            x += p
        x += c if i & 1 else p
        c = sha(kind, x)
    out = ""
    for t in (ORDER256 if kind == 5 else ORDER512):
        out += b64_24(c[t[0]], c[t[1]], c[t[2]], 4)
    if kind == 5:
        out += b64_24(0, c[31], c[30], 3)
    else:
        out += b64_24(0, 0, c[63], 2)
    head = "$%d$" % kind
    if show_rounds:
        head += "rounds=%d$" % rounds
    return head + salt.decode() + "$" + out


# crypt() as libxcrypt does it for the methods here; "*0" on a setting
# it refuses.
def crypt(pw, setting):
    if setting.startswith("$1$"):
        salt = setting[3:].split("$")[0]
        return md5_crypt(pw, salt, "$1$")
    if setting.startswith("$5$") or setting.startswith("$6$"):
        kind = int(setting[1])
        rest = setting[3:]
        rounds, show = 5000, False
        if rest.startswith("rounds="):
            num, _, rest = rest[7:].partition("$")
            if not is_int(num) or int(num) < 1000 or int(num) > 999999999:
                return "*0"
            rounds, show = int(num), True
        return sha_crypt(kind, pw, rest.split("$")[0], rounds, show)
    return "*0"


# ------------------------------------------------------------ account files


class Pw:
    def __init__(self, line):
        self.line = line
        f = line.split(":")
        self.ok = len(f) == 7 and f[0] != "" and is_int(f[2]) and is_int(f[3])
        if self.ok:
            self.name, self.passwd, self.uid, self.gid = f[0], f[1], int(f[2]), int(f[3])
            self.gecos, self.home, self.shell = f[4], f[5], f[6]


def opt_num(s):
    return -1 if s == "" else int(s)


class Sp:
    def __init__(self, line):
        self.line = line
        f = line.split(":")
        self.ok = len(f) == 9 and f[0] != "" and all(x == "" or is_int(x) for x in f[2:8])
        if self.ok:
            self.name, self.passwd = f[0], f[1]
            self.lstchg, self.min, self.max, self.warn, self.inact, self.expire = (opt_num(x) for x in f[2:8])
            self.flag = f[8]

    def text(self):
        nums = [self.lstchg, self.min, self.max, self.warn, self.inact, self.expire]
        return ":".join([self.name, self.passwd] + ["" if n < 0 else str(n) for n in nums] + [self.flag])


def read_lines(path):
    text = open(path).read()
    lines = text.split("\n")
    if lines and lines[-1] == "":
        lines.pop()
    return lines


def load_pw(root):
    return [Pw(l) for l in read_lines(root + "/etc/passwd")]


def load_sp(root):
    if not os.path.exists(root + "/etc/shadow"):
        return []
    return [Sp(l) for l in read_lines(root + "/etc/shadow")]


def save_sp(root, sps):
    open(root + "/etc/shadow", "w").write("".join(s.line + "\n" for s in sps))


def find_pw(pws, name):
    for p in pws:
        if p.ok and p.name == name:
            return p
    return None


def find_sp(sps, name):
    for s in sps:
        if s.ok and s.name == name:
            return s
    return None


def exists(root, path):
    return os.path.lexists(root + path)


class Job:
    def __init__(self):
        self.out, self.err, self.status = "", "", 0


# ------------------------------------------------------------ pwck


def pwck(job, args):
    root, quiet = "", False
    i = 0
    while i < len(args):
        if args[i] == "-R":
            root = args[i + 1]
            i += 2
            continue
        if args[i] == "-q":
            quiet = True
        i += 1
    pws, sps = load_pw(root), load_sp(root)
    gids = set()
    for l in read_lines(root + "/etc/group"):
        f = l.split(":")
        if len(f) == 4 and is_int(f[2]):
            gids.add(int(f[2]))
    errors = 0
    for p in pws:
        if not p.ok:
            job.out += "invalid password file entry\ndelete line '%s'? No\n" % p.line
            errors += 1
            continue
        dup = False
        for q in pws:
            if q is not p and q.ok and q.name == p.name:
                dup = True
        if dup:
            job.out += "duplicate password entry\ndelete line '%s'? No\n" % p.line
            errors += 1
        else:
            if not quiet and p.gid not in gids:
                job.out += "user '%s': no group %d\n" % (p.name, p.gid)
                errors += 1
            if not quiet and not exists(root, p.home):
                job.out += "user '%s': directory '%s' does not exist\n" % (p.name, p.home)
                errors += 1
            if not quiet and p.shell != "" and not exists(root, p.shell):
                job.out += "user '%s': program '%s' does not exist\n" % (p.name, p.shell)
        if find_sp(sps, p.name) is None:
            job.out += "no matching password file entry in /etc/shadow\nadd user '%s' in /etc/shadow? No\n" % p.name
            errors += 1
    for s in sps:
        if not s.ok:
            job.out += "invalid shadow password file entry\ndelete line '%s'? No\n" % s.line
            errors += 1
            continue
        dup = False
        for t in sps:
            if t is not s and t.ok and t.name == s.name:
                dup = True
        if dup:
            job.out += "duplicate shadow password entry\ndelete line '%s'? No\n" % s.line
            errors += 1
            continue
        if find_pw(pws, s.name) is None:
            job.out += "no matching password file entry in /etc/passwd\ndelete line '%s'? No\n" % s.line
            errors += 1
            continue
        if not quiet and s.lstchg > TODAY:
            job.out += "user %s: last password change in the future\n" % s.name
    job.out += "pwck: no changes\n"
    if errors > 0:
        job.status = 2


# ------------------------------------------------------------ chage


class Usage(Exception):
    pass


def chage(job, args):
    root, listing, iso = "", False, False
    sets = {}
    login = None
    i = 0
    while i < len(args):
        a = args[i]
        if a == "-R":
            root = args[i + 1]
            i += 2
        elif a == "-l":
            listing = True
            i += 1
        elif a == "-i":
            iso = True
            i += 1
        elif a in ("-d", "-E"):
            v = str_to_day(args[i + 1])
            if v is None:
                job.err += "chage: invalid date '%s'\n" % args[i + 1] + usage.CHAGE
                job.status = 2
                return
            sets[a] = v
            i += 2
        elif a in ("-I", "-m", "-M", "-W"):
            v = parse_long(args[i + 1])
            if v is None:
                job.err += "chage: invalid numeric argument '%s'\n" % args[i + 1] + usage.CHAGE
                job.status = 2
                return
            sets[a] = v
            i += 2
        else:
            login = a
            i += 1
    if login is None:
        job.err += usage.CHAGE
        job.status = 2
        return
    pws, sps = load_pw(root), load_sp(root)
    if find_pw(pws, login) is None:
        job.err += "chage: user '%s' does not exist in /etc/passwd\n" % login
        job.status = 1
        return
    sp = find_sp(sps, login)
    if listing:
        if sp is None:
            lst = mn = mx = wn = ina = exp = -1
        else:
            lst, mn, mx, wn, ina, exp = sp.lstchg, sp.min, sp.max, sp.warn, sp.inact, sp.expire
        date = iso_date if iso else long_date
        out = "Last password change\t\t\t\t\t: "
        if lst < 0:
            out += "never\n"
        elif lst == 0:
            out += "password must be changed\n"
        else:
            out += date(lst) + "\n"
        out += "Password expires\t\t\t\t\t: "
        if lst == 0:
            out += "password must be changed\n"
        elif lst < 0 or mx >= 10000 or mx < 0:
            out += "never\n"
        else:
            out += date(lst + mx) + "\n"
        out += "Password inactive\t\t\t\t\t: "
        if lst == 0:
            out += "password must be changed\n"
        elif lst < 0 or ina < 0 or mx >= 10000 or mx < 0:
            out += "never\n"
        else:
            out += date(lst + mx + ina) + "\n"
        out += "Account expires\t\t\t\t\t\t: "
        out += "never\n" if exp < 0 else date(exp) + "\n"
        out += "Minimum number of days between password change\t\t: %d\n" % mn
        out += "Maximum number of days between password change\t\t: %d\n" % mx
        out += "Number of days of warning before password expires\t: %d\n" % wn
        job.out += out
        return
    if "-d" in sets:
        sp.lstchg = sets["-d"]
    if "-E" in sets:
        sp.expire = sets["-E"]
    if "-I" in sets:
        sp.inact = sets["-I"]
    if "-m" in sets:
        sp.min = sets["-m"]
    if "-M" in sets:
        sp.max = sets["-M"]
    if "-W" in sets:
        sp.warn = sets["-W"]
    sp.line = sp.text()
    save_sp(root, sps)


# ------------------------------------------------------------ passwd


def pw_status(p):
    if p.startswith("!") or p.startswith("*"):
        return "L"
    if p == "":
        return "NP"
    return "P"


def status_line(p, sps):
    sp = find_sp(sps, p.name)
    if sp is None:
        return "%s %s\n" % (p.name, pw_status(p.passwd))
    if sp.lstchg < 0:
        return "%s %s\n" % (p.name, pw_status(sp.passwd))
    return "%s %s %s %d %d %d %d\n" % (p.name, pw_status(sp.passwd), iso_date(sp.lstchg),
                                       sp.min, sp.max, sp.warn, sp.inact)


def passwd(job, args):
    root, action, all_users, login = "", "", False, None
    sets = {}
    i = 0
    while i < len(args):
        a = args[i]
        if a == "-R":
            root = args[i + 1]
            i += 2
        elif a in ("-n", "-x", "-w", "-i"):
            v = parse_long(args[i + 1])
            if v is None:
                job.err += "passwd: invalid numeric argument '%s'\n" % args[i + 1] + usage.PASSWD
                job.status = 6
                return
            sets[a] = v
            i += 2
        elif a == "-a":
            all_users = True
            i += 1
        elif a in ("-S", "-l", "-u", "-d", "-e"):
            action = a
            i += 1
        else:
            login = a
            i += 1
    pws, sps = load_pw(root), load_sp(root)
    if action == "-S" and all_users:
        for p in pws:
            if p.ok:
                job.out += status_line(p, sps)
        return
    if login is None:
        job.err += usage.PASSWD
        job.status = 2
        return
    p = find_pw(pws, login)
    if p is None:
        job.err += "passwd: user '%s' does not exist\n" % login
        job.status = 1
        return
    if action == "-S":
        job.out += status_line(p, sps)
        return
    sp = find_sp(sps, login)
    if action == "-l":
        if not sp.passwd.startswith("!"):
            sp.passwd = "!" + sp.passwd
    elif action == "-u":
        if sp.passwd.startswith("!"):
            if len(sp.passwd) == 1:
                job.err += ("passwd: unlocking the password would result in a passwordless account.\n"
                            "You should set a password with usermod -p to unlock the password of this account.\n")
                job.status = 3
                return
            sp.passwd = sp.passwd[1:]
    elif action == "-d":
        sp.passwd = ""
    elif action == "-e":
        sp.lstchg = 0
    if "-n" in sets:
        sp.min = sets["-n"]
    if "-x" in sets:
        sp.max = sets["-x"]
    if "-w" in sets:
        sp.warn = sets["-w"]
    if "-i" in sets:
        sp.inact = sets["-i"]
    sp.line = sp.text()
    save_sp(root, sps)
    job.out += "passwd: password changed.\n"


# ------------------------------------------------------------ login


# pam_unix: the password, then the account's aging, as of TODAY.
def login(job, args):
    root, user, password = args
    pws, sps = load_pw(root), load_sp(root)
    p = find_pw(pws, user)
    if p is None:
        job.out += "Login incorrect\n"
        job.status = 1
        return
    sp = find_sp(sps, user)
    if p.passwd == "x":
        if sp is None:
            job.out += "Login incorrect\n"
            job.status = 1
            return
        hashed = sp.passwd
    else:
        hashed = p.passwd
        sp = None
    if hashed != "":
        if hashed.startswith("$y$"):
            job.err += "login: yescrypt hashes are not supported\n"
            job.out += "Login incorrect\n"
            job.status = 1
            return
        if crypt(password, hashed) != hashed:
            job.out += "Login incorrect\n"
            job.status = 1
            return
    if sp is not None:
        if sp.expire >= 0 and TODAY >= sp.expire:
            job.out += "Your account has expired; please contact your system administrator.\n"
            job.status = 1
            return
        if sp.lstchg == 0:
            job.out += "You are required to change your password immediately (administrator enforced).\n"
            job.status = 1
            return
        age = TODAY - sp.lstchg
        if sp.lstchg > 0 and sp.max >= 0 and sp.inact >= 0 and age > sp.max + sp.inact:
            job.out += "Your account has expired; please contact your system administrator.\n"
            job.status = 1
            return
        if sp.lstchg > 0 and sp.max >= 0 and age > sp.max:
            job.out += "You are required to change your password immediately (password expired).\n"
            job.status = 1
            return
        if sp.lstchg > 0 and sp.max >= 0 and sp.warn >= 0 and age > sp.max - sp.warn:
            left = sp.lstchg + sp.max - TODAY
            job.out += "Warning: your password will expire in %d %s.\n" % (left, "day" if left == 1 else "days")
    if p.shell.endswith("/nologin"):
        job.out += "This account is currently not available.\n"
        job.status = 1
        return
    job.out += "Welcome, %s (home %s, shell %s)\n" % (user, p.home, p.shell)


# ------------------------------------------------------------ openssl, crypt, htpasswd


def openssl(job, args):
    # openssl passwd -1|-apr1|-5|-6 -salt SALT PASSWORD
    kind, salt, pw = args[1], args[3], args[4]
    if kind in ("-1", "-apr1"):
        job.out += md5_crypt(pw, salt, "$1$" if kind == "-1" else "$apr1$") + "\n"
    elif salt == "":
        job.out += "<NULL>\n"
    else:
        job.out += sha_crypt(int(kind[1]), pw, salt, 5000, False) + "\n"


def htpasswd(job, args):
    # htpasswd -vb FILE USER PASSWORD
    path, user, pw = args[1], args[2], args[3]
    for line in read_lines(path):
        if line.startswith("#") or ":" not in line:
            continue
        name, _, hashed = line.partition(":")
        if name != user:
            continue
        if hashed.startswith("$2y$"):
            job.err += "htpasswd: bcrypt hashes are not supported\n"
            job.status = 3
            return
        if hashed.startswith("$apr1$"):
            good = md5_crypt(pw, hashed[6:].split("$")[0], "$apr1$") == hashed
        elif hashed.startswith("{SHA}"):
            good = "{SHA}" + base64.b64encode(hashlib.sha1(pw.encode()).digest()).decode() == hashed
        else:
            good = crypt(pw, hashed) == hashed
        if good:
            job.err += "Password for user %s correct.\n" % user
        else:
            job.err += "password verification failed\n"
            job.status = 3
        return
    job.err += "User %s not found\n" % user
    job.status = 6


# ------------------------------------------------------------ the run


def build_roots():
    for r in ("sys", "broken"):
        for line in read_lines("data/%s/layout.txt" % r):
            kind, path = line.split(" ")
            full = "data/%s/%s" % (r, path)
            if kind == "d":
                os.makedirs(full, exist_ok=True)
            else:
                os.makedirs(os.path.dirname(full), exist_ok=True)
                open(full, "w").close()


def run_line(job, line):
    w = shell_words(line)
    if w[0] == "pwck":
        pwck(job, w[1:])
    elif w[0] == "chage":
        chage(job, w[1:])
    elif w[0] == "passwd":
        passwd(job, w[1:])
    elif w[0] == "login":
        login(job, w[1:])
    elif w[0] == "openssl":
        openssl(job, w[1:])
    elif w[0] == "crypt":
        job.out += crypt(w[1], w[2]) + "\n"
    elif w[0] == "htpasswd":
        htpasswd(job, w[1:])
    elif w[0] == "cat":
        job.out += open(w[1]).read()


def main():
    if os.path.isdir("data"):
        shutil.rmtree("data")
    mk.write_all()
    build_roots()
    for line in read_lines("data/jobs.txt"):
        if line == "" or line.startswith("#"):
            continue
        job = Job()
        run_line(job, line)
        out = "$ " + line + "\n" + job.out + job.err
        if job.status != 0:
            out += "[exit status %d]\n" % job.status
        print(out)


if __name__ == "__main__":
    main()
