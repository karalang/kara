"""The Python mirror of grade_close.kara: a school district's spring term
close at four high schools. It writes expected.out.

Run: python3 -I -B grade_close.py > expected.out
     GRADE_REVERSE=1 python3 -I -B grade_close.py   # the schools nominate in reverse"""
import os

MASK32 = 4294967295
PER_GRADE = 60
GRADES = [9, 10, 11, 12]
# The core subjects: a course a grade, two sections each, the second
# honors in grades 9 and 10 and AP in 11 and 12.
CORE = ["ENG", "MATH", "SCI", "HIST"]
CORE_TITLES = [
    ["English 9", "English 10", "English 11", "English 12"],
    ["Algebra 1", "Geometry", "Algebra 2", "Precalculus"],
    ["Biology", "Chemistry", "Physics", "Environmental Science"],
    ["World History", "US History", "Government", "Economics"],
]
# The electives, any grade, with their credits in tenths and sections.
ELECTIVES = [("SPAN", "Spanish", 10, 3), ("FREN", "French", 10, 2), ("ART", "Studio Art", 5, 3),
             ("PE", "Physical Education", 5, 4), ("CS", "Computer Science", 10, 2)]
CORE_CAP = 34
ELECTIVE_CAP = 40
PERIODS = 7
# Gradebook columns: six homeworks out of 10, three quizzes out of 20,
# two tests out of 100 and a final out of 150.
ITEMS = ["h1", "h2", "h3", "h4", "h5", "h6", "q1", "q2", "q3", "t1", "t2", "final"]
ITEM_MAX = [10, 10, 10, 10, 10, 10, 20, 20, 20, 100, 100, 150]
ITEM_CAT = [0, 0, 0, 0, 0, 0, 1, 1, 1, 2, 2, 3]
CATS = ["homework", "quizzes", "tests", "final"]
WEIGHTS = [(30, 20, 30, 20), (20, 20, 40, 20), (25, 15, 35, 25), (40, 10, 30, 20)]
LETTERS = ["A", "A-", "B+", "B", "B-", "C+", "C", "C-", "D+", "D", "D-", "F"]
CUTS = [93, 90, 87, 83, 80, 77, 73, 70, 67, 63, 60, 0]
POINTS = [400, 370, 330, 300, 270, 230, 200, 170, 130, 100, 70, 0]
BONUS = {"REG": 0, "HON": 50, "AP": 100}
MAX_ABSENCES = 15
GRAD_CREDITS = 220
FIRST = ["Avery", "Blake", "Casey", "Devon", "Elliot", "Finley", "Gray", "Harper", "Indy", "Jordan", "Kai",
         "Logan", "Morgan", "Noel", "Oakley", "Parker", "Quinn", "Riley", "Sage", "Taylor", "Umi", "Vale",
         "Wren", "Yael"]
LAST = ["Abara", "Bishop", "Chen", "Delgado", "Eze", "Fischer", "Garcia", "Haddad", "Ivanova", "Jensen",
        "Kato", "Lindqvist", "Moreau", "Nakamura", "Okafor", "Patel", "Quispe", "Rossi", "Silva", "Tanaka",
        "Ueda", "Varga", "Walsh", "Zhou"]
# The district's scholarship fund, in cents: each school nominates up to
# five seniors, and the fund pays them in the order they are nominated.
FUND = 6000000
AWARD_TOP = 500000
AWARD = 300000
AWARD_MIN = 100000
MAX_NOMINEES = 5
NOMINEE_GPA = 350


class School:
    def __init__(self, code, digit, seed, abil, miss, absent):
        self.code = code
        self.digit = digit
        self.seed = seed
        self.abil = abil
        self.miss = miss
        self.absent = absent


class Student:
    def __init__(self, sid, last, first, grade, absences, prior_cr, prior_gpa):
        self.sid = sid
        self.last = last
        self.first = first
        self.grade = grade
        self.absences = absences
        self.prior_cr = prior_cr
        self.prior_gpa = prior_gpa

    def key(self):
        return (self.sid, self.last, self.first, self.grade, self.absences, self.prior_cr, self.prior_gpa)


class Section:
    def __init__(self, sec, course, title, teacher, period, level, credits, w, drop, curve):
        self.sec = sec
        self.course = course
        self.title = title
        self.teacher = teacher
        self.period = period
        self.level = level
        self.credits = credits
        self.w = list(w)
        self.drop = drop
        self.curve = curve

    def key(self):
        return (self.sec, self.course, self.title, self.teacher, self.period, self.level, self.credits,
                tuple(self.w), self.drop, self.curve)


class Row:
    # one student's gradebook row in one section; -1 missing, -2 excused
    def __init__(self, sid, sec, scores):
        self.sid = sid
        self.sec = sec
        self.scores = list(scores)

    def key(self):
        return (self.sid, self.sec, tuple(self.scores))


class BatchError(Exception):
    pass


# ------------------------------------------------------------ helpers

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


def group3(n):
    s = str(n)
    out = ""
    while len(s) > 3:
        out = "," + s[len(s) - 3:] + out
        s = s[:len(s) - 3]
    return s + out


def money(c):
    return "$%s.%02d" % (group3(c // 100), c % 100)


def dollars(c):
    return "%d.%02d" % (c // 100, c % 100)


def hundredths(v):
    return "%d.%02d" % (v // 100, v % 100)


def tenths(v):
    return "%d.%d" % (v // 10, v % 10)


def is_digits(s):
    return len(s) > 0 and all("0" <= ch <= "9" for ch in s)


def small_int(t, lo, hi):
    if not is_digits(t) or len(t) > 6:
        return None
    v = int(t)
    if v < lo or v > hi:
        return None
    return v


def decimal_of(t, places):
    dot = t.find(".")
    if dot < 1 or len(t) - dot != places + 1:
        return None
    a = t[:dot]
    b = t[dot + 1:]
    if not (is_digits(a) and is_digits(b)) or len(a) > 3:
        return None
    return int(a) * (10 ** places) + int(b)


def name_ok(t):
    return len(t) > 0 and all(("A" <= ch <= "Z") or ("a" <= ch <= "z") for ch in t)


def file_line(name, text):
    rows = len(text.split("\n")) - 2
    return "%s: %s rows, %s bytes, fnv %08x" % (name, group3(rows), group3(len(text)), text_sum(text))


def avg_tenths(total, n):
    if n == 0:
        return "0.0"
    return tenths(total * 10 // n)


# ------------------------------------------------------------ building

def build_sections(school):
    out = []
    n = 0
    for g in range(4):
        for s in range(4):
            for j in range(2):
                level = "REG"
                if j == 1:
                    level = "HON" if g < 2 else "AP"
                out.append(new_section(school, n, CORE[s] + str(GRADES[g]), CORE_TITLES[s][g], level, 10))
                n += 1
    for e in ELECTIVES:
        for j in range(e[3]):
            out.append(new_section(school, n, e[0], e[1], "REG", e[2]))
            n += 1
    return out


def new_section(school, n, course, title, level, credits):
    r = mix(school.seed, n, 11)
    teacher = "%s. %s" % (LAST[mix(school.seed, n, 12) % 24][0], LAST[mix(school.seed, n, 13) % 24])
    w = WEIGHTS[r % 4]
    drop = (r >> 4) % 3
    curve = 75 if (r >> 8) % 10 < 3 else 0
    return Section("%s%d" % (school.code, 100 + n), course, title, teacher, 1 + (r >> 12) % PERIODS, level,
                   credits, w, drop, curve)


def build_students(school):
    out = []
    for g in range(4):
        grade = GRADES[g]
        for k in range(PER_GRADE):
            i = g * 100 + k
            r = mix(school.seed, i, 21)
            sid = school.digit * 100000 + grade * 1000 + k + 1
            if r % 100 < school.absent:
                absences = 16 + (r >> 8) % 15
            else:
                absences = (r >> 8) % 11
            prior_cr = (grade - 9) * 60 + 30
            if (r >> 12) % 4 == 0:
                prior_cr -= 5 * ((r >> 14) % 5)
            prior_gpa = 150 + (r >> 17) % 290
            out.append(Student(sid, LAST[mix(school.seed, i, 22) % 24], FIRST[mix(school.seed, i, 23) % 24],
                               grade, absences, prior_cr, prior_gpa))
    return out


def ability_of(school, i):
    return 63 + mix(school.seed, i, 31) % 36 + school.abil


def build_rows(school, students, sections):
    out = []
    count = [0] * len(sections)
    for i in range(len(students)):
        st = students[i]
        g = st.grade - 9
        a = ability_of(school, i)
        used = [False] * (PERIODS + 1)
        mine = []
        for s in range(4):
            base = (g * 4 + s) * 2
            pref = [base + 1, base] if a >= 85 else [base, base + 1]
            for k in pref:
                sec = sections[k]
                if not used[sec.period] and count[k] < CORE_CAP:
                    used[sec.period] = True
                    count[k] += 1
                    mine.append(k)
                    break
        taken = []
        first = mix(school.seed, i, 32) % len(ELECTIVES)
        for e in range(len(ELECTIVES)):
            if len(taken) == 2:
                break
            course = ELECTIVES[(first + e) % len(ELECTIVES)][0]
            for k in range(32, len(sections)):
                sec = sections[k]
                if sec.course == course and not used[sec.period] and count[k] < ELECTIVE_CAP:
                    used[sec.period] = True
                    count[k] += 1
                    mine.append(k)
                    taken.append(course)
                    break
        mine.sort(key=lambda k: sections[k].period)
        miss = school.miss + mix(school.seed, i, 33) % 60
        for k in mine:
            ease = mix(school.seed, k, 34) % 11 - 5
            scores = []
            for t in range(len(ITEMS)):
                r = mix(school.seed * 7 + i, k * 16 + t, 35)
                if t == 11 and r % 1000 < 4:
                    scores.append(-2)
                elif r % 1000 < miss and t < 9:
                    scores.append(-1)
                elif t < 11 and r % 1000 >= 996:
                    scores.append(-2)
                else:
                    p = a + ease + (r >> 10) % 25 - 12
                    if ITEM_CAT[t] >= 2:
                        p -= 6
                    p = max(0, min(100, p))
                    scores.append(ITEM_MAX[t] * p // 100)
            out.append(Row(st.sid, sections[k].sec, scores))
    return out


# ------------------------------------------------------------ files

def student_header():
    return "student,last,first,grade,absences,prior_credits,prior_gpa"


def section_header():
    return "section,course,title,teacher,period,level,credits,w_hw,w_quiz,w_test,w_final,drop,curve"


def score_header():
    return "student,section," + ",".join(ITEMS)


def encode_students(students):
    out = student_header() + "\n"
    for s in students:
        out += "%d,%s,%s,%d,%d,%s,%s\n" % (s.sid, s.last, s.first, s.grade, s.absences, tenths(s.prior_cr),
                                           hundredths(s.prior_gpa))
    return out


def encode_sections(sections):
    out = section_header() + "\n"
    for c in sections:
        out += "%s,%s,%s,%s,%d,%s,%s,%d,%d,%d,%d,%d,%d\n" % (c.sec, c.course, c.title, c.teacher, c.period, c.level,
                                                          tenths(c.credits), c.w[0], c.w[1], c.w[2], c.w[3],
                                                          c.drop, c.curve)
    return out


def score_text(v):
    if v == -1:
        return "M"
    if v == -2:
        return "X"
    return str(v)


def encode_rows(rows):
    out = score_header() + "\n"
    for r in rows:
        out += "%d,%s,%s\n" % (r.sid, r.sec, ",".join(score_text(v) for v in r.scores))
    return out


def read_text(path):
    try:
        with open(path) as f:
            return f.read()
    except OSError:
        raise BatchError("cannot read %s" % path)


def write_text(path, text):
    with open(path, "w") as f:
        f.write(text)


def damage(text, n, field, value):
    lines = text.split("\n")
    f = lines[n].split(",")
    f[field] = value
    lines[n] = ",".join(f)
    return "\n".join(lines)


# ------------------------------------------------------------ parsing

def body_lines(path, text, header):
    lines = text.split("\n")
    n = len(lines)
    if n < 2 or lines[n - 1] != "":
        raise BatchError("%s: no final newline" % path)
    if lines[0] != header:
        raise BatchError("%s line 1: bad header" % path)
    return lines[1:n - 1]


def fields_of(path, n, line, want):
    f = line.split(",")
    if len(f) != want:
        raise BatchError("%s line %d: expected %d fields, found %d" % (path, n, want, len(f)))
    return f


def int_field(path, n, what, t, lo, hi):
    v = small_int(t, lo, hi)
    if v is None:
        raise BatchError("%s line %d: bad %s '%s'" % (path, n, what, t))
    return v


def parse_students(path, school, text):
    body = body_lines(path, text, student_header())
    out = []
    seen = set()
    for k in range(len(body)):
        n = k + 2
        f = fields_of(path, n, body[k], 7)
        sid = small_int(f[0], 100000, 999999)
        if sid is None or len(f[0]) != 6:
            raise BatchError("%s line %d: bad student '%s'" % (path, n, f[0]))
        if sid // 100000 != school.digit:
            raise BatchError("%s line %d: student %d is not at %s" % (path, n, sid, school.code))
        if sid in seen:
            raise BatchError("%s line %d: student %d appears twice" % (path, n, sid))
        seen.add(sid)
        if not name_ok(f[1]) or not name_ok(f[2]):
            raise BatchError("%s line %d: bad name '%s %s'" % (path, n, f[2], f[1]))
        grade = int_field(path, n, "grade", f[3], 9, 12)
        if sid // 1000 % 100 != grade:
            raise BatchError("%s line %d: student %d is not in grade %d" % (path, n, sid, grade))
        absences = int_field(path, n, "absences", f[4], 0, 180)
        cr = decimal_of(f[5], 1)
        if cr is None or cr > 400:
            raise BatchError("%s line %d: bad prior credits '%s'" % (path, n, f[5]))
        gpa = decimal_of(f[6], 2)
        if gpa is None or gpa > 500:
            raise BatchError("%s line %d: bad prior GPA '%s'" % (path, n, f[6]))
        out.append(Student(sid, f[1], f[2], grade, absences, cr, gpa))
    return out


def course_ok(course):
    for s in CORE:
        for g in GRADES:
            if course == s + str(g):
                return True
    for e in ELECTIVES:
        if course == e[0]:
            return True
    return False


def find_section(sections, sec):
    for i in range(len(sections)):
        if sections[i].sec == sec:
            return i
    return -1


def parse_sections(path, school, text):
    body = body_lines(path, text, section_header())
    out = []
    for k in range(len(body)):
        n = k + 2
        f = fields_of(path, n, body[k], 13)
        sec = f[0]
        if len(sec) != 6 or not is_digits(sec[3:]) or not ("A" <= sec[0] <= "Z"):
            raise BatchError("%s line %d: bad section '%s'" % (path, n, sec))
        if sec[:3] != school.code:
            raise BatchError("%s line %d: section %s is not at %s" % (path, n, sec, school.code))
        if find_section(out, sec) >= 0:
            raise BatchError("%s line %d: section %s appears twice" % (path, n, sec))
        if not course_ok(f[1]):
            raise BatchError("%s line %d: bad course '%s'" % (path, n, f[1]))
        if len(f[2]) == 0 or len(f[3]) == 0:
            raise BatchError("%s line %d: section %s has no title or teacher" % (path, n, sec))
        period = int_field(path, n, "period", f[4], 1, PERIODS)
        if f[5] not in BONUS:
            raise BatchError("%s line %d: bad level '%s'" % (path, n, f[5]))
        credits = decimal_of(f[6], 1)
        if credits is None or (credits != 5 and credits != 10):
            raise BatchError("%s line %d: bad credits '%s'" % (path, n, f[6]))
        w = [int_field(path, n, "weight", f[7 + c], 0, 100) for c in range(4)]
        if sum(w) != 100:
            raise BatchError("%s line %d: the weights of %s add up to %d" % (path, n, sec, sum(w)))
        drop = int_field(path, n, "drop", f[11], 0, 5)
        curve = int_field(path, n, "curve", f[12], 0, 100)
        out.append(Section(sec, f[1], f[2], f[3], period, f[5], credits, w, drop, curve))
    return out


def find_student(students, sid):
    for i in range(len(students)):
        if students[i].sid == sid:
            return i
    return -1


def parse_rows(path, students, sections, text):
    body = body_lines(path, text, score_header())
    out = []
    seen = set()
    periods = set()
    for k in range(len(body)):
        n = k + 2
        f = fields_of(path, n, body[k], 2 + len(ITEMS))
        sid = small_int(f[0], 0, 999999)
        if sid is None or find_student(students, sid) < 0:
            raise BatchError("%s line %d: no student '%s'" % (path, n, f[0]))
        c = find_section(sections, f[1])
        if c < 0:
            raise BatchError("%s line %d: no section '%s'" % (path, n, f[1]))
        if (sid, f[1]) in seen:
            raise BatchError("%s line %d: student %d is in %s twice" % (path, n, sid, f[1]))
        seen.add((sid, f[1]))
        p = sections[c].period
        if (sid, p) in periods:
            raise BatchError("%s line %d: student %d has two sections in period %d" % (path, n, sid, p))
        periods.add((sid, p))
        scores = []
        for t in range(len(ITEMS)):
            v = f[2 + t]
            if v == "M":
                scores.append(-1)
            elif v == "X":
                scores.append(-2)
            else:
                x = small_int(v, 0, 999)
                if x is None or x > ITEM_MAX[t]:
                    raise BatchError("%s line %d: bad %s '%s' (out of %d)" % (path, n, ITEMS[t], v, ITEM_MAX[t]))
                scores.append(x)
        out.append(Row(sid, f[1], scores))
    return out


# ------------------------------------------------------------ the term close

class Term:
    def __init__(self):
        # by section: the curve given to its tests, in points
        self.bump = []
        # by row: the percent (-1 for an incomplete), the letter index
        # (12 for I), and the credits earned
        self.pct = []
        self.letter = []
        self.earned = []
        self.nc = []
        # by student: the section rows, term and cumulative GPAs
        self.rows_of = []
        self.term_gpa = []
        self.term_wgpa = []
        self.cum_wgpa = []
        self.term_cr = []
        self.total_cr = []
        self.fails = []


def letter_of(pct):
    for i in range(len(CUTS)):
        if pct >= CUTS[i]:
            return i
    return 11


def close_term(students, sections, rows):
    term = Term()
    sec_of = [find_section(sections, r.sec) for r in rows]
    st_of = [find_student(students, r.sid) for r in rows]
    for c in range(len(sections)):
        sec = sections[c]
        bump = 0
        if sec.curve > 0:
            got = 0
            out_of = 0
            for j in range(len(rows)):
                if sec_of[j] == c:
                    for t in (9, 10):
                        if rows[j].scores[t] >= 0:
                            got += rows[j].scores[t]
                            out_of += ITEM_MAX[t]
            if out_of > 0:
                mean = got * 100 // out_of
                if mean < sec.curve:
                    bump = sec.curve - mean
        term.bump.append(bump)
    for j in range(len(rows)):
        r = rows[j]
        sec = sections[sec_of[j]]
        st = students[st_of[j]]
        if r.scores[11] == -2:
            term.pct.append(-1)
            term.letter.append(12)
            term.earned.append(0)
            term.nc.append(False)
            continue
        got = [[], [], [], []]
        for t in range(len(ITEMS)):
            v = r.scores[t]
            if v == -2:
                continue
            if v == -1:
                v = 0
            elif ITEM_CAT[t] == 2:
                v = min(ITEM_MAX[t], v + term.bump[sec_of[j]])
            got[ITEM_CAT[t]].append((v, ITEM_MAX[t]))
        hw = sorted(got[0], key=lambda x: x[0])
        drop = min(sec.drop, max(0, len(hw) - 1))
        got[0] = hw[drop:]
        num = 0
        den = 0
        for c in range(4):
            if len(got[c]) == 0 or sec.w[c] == 0:
                continue
            p = sum(x[0] for x in got[c])
            m = sum(x[1] for x in got[c])
            num += sec.w[c] * (p * 10000 // m)
            den += sec.w[c]
        v = num // den
        pct = (v + 50) // 100
        li = letter_of(pct)
        term.pct.append(pct)
        term.letter.append(li)
        nc = st.absences > MAX_ABSENCES
        term.nc.append(nc)
        term.earned.append(0 if (nc or li == 11) else sec.credits)
    for i in range(len(students)):
        term.rows_of.append([])
    for j in range(len(rows)):
        term.rows_of[st_of[j]].append(j)
    for i in range(len(students)):
        st = students[i]
        qp = 0
        wqp = 0
        cr = 0
        earned = 0
        fails = 0
        for j in term.rows_of[i]:
            li = term.letter[j]
            earned += term.earned[j]
            if li == 12:
                continue
            sec = sections[sec_of[j]]
            gp = POINTS[li]
            wgp = gp + (BONUS[sec.level] if gp >= 200 else 0)
            qp += gp * sec.credits
            wqp += wgp * sec.credits
            cr += sec.credits
            if li == 11:
                fails += 1
        term.term_gpa.append((2 * qp + cr) // (2 * cr) if cr > 0 else 0)
        term.term_wgpa.append((2 * wqp + cr) // (2 * cr) if cr > 0 else 0)
        all_cr = st.prior_cr + cr
        all_qp = st.prior_gpa * st.prior_cr + wqp
        term.cum_wgpa.append((2 * all_qp + all_cr) // (2 * all_cr))
        term.term_cr.append(earned)
        term.total_cr.append(st.prior_cr + earned)
        term.fails.append(fails)
    return term


def letter_text(li):
    return "I" if li == 12 else LETTERS[li]


def encode_cards(students, sections, rows, term):
    out = "student,section,course,level,percent,letter,credits\n"
    for j in range(len(rows)):
        r = rows[j]
        sec = sections[find_section(sections, r.sec)]
        pct = "-" if term.pct[j] < 0 else str(term.pct[j])
        credits = "NC" if term.nc[j] else tenths(term.earned[j])
        out += "%d,%s,%s,%s,%s,%s,%s\n" % (r.sid, r.sec, sec.course, sec.level, pct, letter_text(term.letter[j]),
                                           credits)
    return out


def encode_gpas(students, term):
    out = "student,term_gpa,weighted,cumulative,credits\n"
    for i in range(len(students)):
        out += "%d,%s,%s,%s,%s\n" % (students[i].sid, hundredths(term.term_gpa[i]), hundredths(term.term_wgpa[i]),
                                     hundredths(term.cum_wgpa[i]), tenths(term.total_cr[i]))
    return out


# ------------------------------------------------------------ reports

def grade_report(school, rows, term):
    counts = [0] * 13
    nc = 0
    for j in range(len(rows)):
        counts[term.letter[j]] += 1
        if term.nc[j]:
            nc += 1
    parts = ["%s %d" % (letter_text(i), counts[i]) for i in range(13)]
    return "%s grades: %s section grades: %s; %d NC" % (school.code, group3(len(rows)), ", ".join(parts), nc)


def section_report(school, sections, rows, term):
    total = [0] * len(sections)
    n = [0] * len(sections)
    fails = [0] * len(sections)
    for j in range(len(rows)):
        c = find_section(sections, rows[j].sec)
        if term.pct[j] >= 0:
            total[c] += term.pct[j]
            n[c] += 1
        if term.letter[j] == 11:
            fails[c] += 1
    curved = 0
    bumps = 0
    for c in range(len(sections)):
        if term.bump[c] > 0:
            curved += 1
            bumps += term.bump[c]
    lo = -1
    hi = -1
    for c in range(len(sections)):
        if n[c] == 0:
            continue
        if lo < 0 or total[c] * n[lo] < total[lo] * n[c]:
            lo = c
        if hi < 0 or total[c] * n[hi] > total[hi] * n[c]:
            hi = c
    worst = sorted(range(len(sections)), key=lambda c: (-fails[c], c))[:3]
    lines = ["%s sections: %d, %d curved (average +%s), lowest %s %s %s%%, highest %s %s %s%%" % (
        school.code, len(sections), curved, avg_tenths(bumps, curved),
        sections[lo].sec, sections[lo].title, avg_tenths(total[lo], n[lo]),
        sections[hi].sec, sections[hi].title, avg_tenths(total[hi], n[hi]))]
    parts = ["%s %s (%s) %d of %d" % (sections[c].sec, sections[c].title, sections[c].teacher, fails[c], n[c])
             for c in worst]
    lines.append("%s most Fs: %s" % (school.code, ", ".join(parts)))
    return lines


def honor_report(school, students, term):
    high = [0] * 4
    hon = [0] * 4
    for i in range(len(students)):
        lows = [term.letter[j] for j in term.rows_of[i]]
        if len(lows) == 0 or 12 in lows:
            continue
        worst = max(lows)
        g = students[i].grade - 9
        if term.term_wgpa[i] >= 375 and worst <= 3:
            high[g] += 1
        elif term.term_wgpa[i] >= 325 and worst <= 6:
            hon[g] += 1
    by = ", ".join("%d: %d/%d" % (GRADES[g], high[g], hon[g]) for g in range(4))
    top = sorted(range(len(students)), key=lambda i: (-term.term_wgpa[i], students[i].sid))[:3]
    names = ", ".join("%s %s %s" % (students[i].first, students[i].last, hundredths(term.term_wgpa[i]))
                      for i in top)
    return "%s honor roll: %d high honors, %d honors (by grade %s); top term %s" % (
        school.code, sum(high), sum(hon), by, names)


def risk_report(school, students, term):
    two_f = 0
    nc = 0
    short = []
    for i in range(len(students)):
        if term.fails[i] >= 2:
            two_f += 1
        if students[i].absences > MAX_ABSENCES:
            nc += 1
        if students[i].grade == 12 and term.total_cr[i] < GRAD_CREDITS:
            short.append(i)
    ids = ", ".join("%d (%s)" % (students[i].sid, tenths(term.total_cr[i])) for i in short)
    if len(short) == 0:
        ids = "none"
    return "%s at risk: %d with two or more Fs, %d over %d absences; seniors short of %s credits: %d: %s" % (
        school.code, two_f, nc, MAX_ABSENCES, tenths(GRAD_CREDITS), len(short), ids)


def rank_order(students, term):
    seniors = [i for i in range(len(students)) if students[i].grade == 12]
    return sorted(seniors, key=lambda i: (-term.cum_wgpa[i], students[i].sid))


def rank_report(school, students, term):
    order = rank_order(students, term)
    titles = ["valedictorian", "salutatorian", "third"]
    parts = ["%s %s %s %s" % (titles[k], students[order[k]].first, students[order[k]].last,
                              hundredths(term.cum_wgpa[order[k]])) for k in range(3)]
    return "%s class of 2027: %s" % (school.code, ", ".join(parts))


class Fund:
    def __init__(self):
        self.left = FUND
        self.log = []


def nominate(fund, school, students, term):
    picks = []
    for i in rank_order(students, term):
        if len(picks) == MAX_NOMINEES:
            break
        if term.cum_wgpa[i] < NOMINEE_GPA or term.fails[i] > 0 or term.total_cr[i] < GRAD_CREDITS:
            continue
        if any(term.letter[j] == 12 for j in term.rows_of[i]):
            continue
        picks.append(i)
    lines = []
    paid = 0
    unpaid = 0
    for i in picks:
        st = students[i]
        want = AWARD_TOP if term.cum_wgpa[i] >= 400 else AWARD
        amount = min(want, fund.left)
        if amount < AWARD_MIN:
            unpaid += 1
            fund.log.append("%s,%d,%s %s,0.00,%s" % (school.code, st.sid, st.first, st.last, dollars(fund.left)))
            continue
        fund.left -= amount
        paid += amount
        fund.log.append("%s,%d,%s %s,%s,%s" % (school.code, st.sid, st.first, st.last, dollars(amount),
                                              dollars(fund.left)))
        lines.append("%s scholarship: %s %s (%s) %s" % (school.code, st.first, st.last,
                                                         hundredths(term.cum_wgpa[i]), money(amount)))
    lines.append("%s scholarships: %d nominated, %s paid, %d unpaid, %s left in the fund" % (
        school.code, len(picks), money(paid), unpaid, money(fund.left)))
    return lines


# ------------------------------------------------------------ the job

def run():
    schools = [
        School("NHS", 1, 8101, 0, 20, 6),
        School("EHS", 2, 8202, 3, 15, 4),
        School("SHS", 3, 8303, -4, 40, 10),
        School("WHS", 4, 8404, 1, 25, 7),
    ]
    secs = [build_sections(s) for s in schools]
    studs = [build_students(s) for s in schools]
    books = [build_rows(schools[i], studs[i], secs[i]) for i in range(4)]
    names = []
    for s in schools:
        names += [s.code + "_students.csv", s.code + "_sections.csv", s.code + "_scores.csv"]
    for i in range(4):
        write_text(names[i * 3], encode_students(studs[i]))
        write_text(names[i * 3 + 1], encode_sections(secs[i]))
        write_text(names[i * 3 + 2], encode_rows(books[i]))
    whole = read_text("NHS_students.csv")
    write_text("NHS_students.bad", damage(whole, 40, 3, "13"))
    whole2 = read_text("SHS_scores.csv")
    write_text("SHS_scores.bad", damage(whole2, 200, 4, "11"))
    texts = [read_text(p) for p in names]
    labels = []
    for s in schools:
        labels += [s.code + " students", s.code + " sections", s.code + " scores"]
    for k in range(12):
        print(file_line(labels[k], texts[k]))
    st = [parse_students(names[i * 3], schools[i], texts[i * 3]) for i in range(4)]
    sc = [parse_sections(names[i * 3 + 1], schools[i], texts[i * 3 + 1]) for i in range(4)]
    rw = [parse_rows(names[i * 3 + 2], st[i], sc[i], texts[i * 3 + 2]) for i in range(4)]
    try:
        parse_students("NHS_students.bad", schools[0], read_text("NHS_students.bad"))
        print("NHS_students.bad: parsed")
    except BatchError as e:
        print(e)
    try:
        parse_rows("SHS_scores.bad", st[2], sc[2], read_text("SHS_scores.bad"))
        print("SHS_scores.bad: parsed")
    except BatchError as e:
        print(e)
    try:
        parse_sections("WHS_sections.csv", schools[1], texts[10])
        print("WHS_sections.csv: parsed")
    except BatchError as e:
        print(e)
    same = all([a.key() for a in st[i]] == [b.key() for b in studs[i]] and
               [a.key() for a in sc[i]] == [b.key() for b in secs[i]] and
               [a.key() for a in rw[i]] == [b.key() for b in books[i]] for i in range(4))
    print("parsed files match the built ones: %s" % ("true" if same else "false"))
    terms = [close_term(st[i], sc[i], rw[i]) for i in range(4)]
    for i in range(4):
        write_text(schools[i].code + "_cards.csv", encode_cards(st[i], sc[i], rw[i], terms[i]))
    for i in range(4):
        write_text(schools[i].code + "_gpa.csv", encode_gpas(st[i], terms[i]))
    for i in range(4):
        print(file_line(schools[i].code + " report cards", read_text(schools[i].code + "_cards.csv")))
        print(file_line(schools[i].code + " GPAs", read_text(schools[i].code + "_gpa.csv")))
    for i in range(4):
        print(grade_report(schools[i], rw[i], terms[i]))
    for i in range(4):
        for line in section_report(schools[i], sc[i], rw[i], terms[i]):
            print(line)
    for i in range(4):
        print(honor_report(schools[i], st[i], terms[i]))
    for i in range(4):
        print(risk_report(schools[i], st[i], terms[i]))
    for i in range(4):
        print(rank_report(schools[i], st[i], terms[i]))
    fund = Fund()
    seq = [0, 1, 2, 3]
    if os.environ.get("GRADE_REVERSE") == "1":
        seq = [3, 2, 1, 0]
    for i in seq:
        for line in nominate(fund, schools[i], st[i], terms[i]):
            print(line)
    write_text("awards.log", "\n".join(fund.log) + "\n")
    back = read_text("awards.log")
    rows = len(back.split("\n")) - 1
    print("awards.log: %s entries, %s bytes, fnv %08x" % (group3(rows), group3(len(back)), text_sum(back)))
    for p in names + ["NHS_students.bad", "SHS_scores.bad", "awards.log"] + \
            [s.code + "_cards.csv" for s in schools] + [s.code + "_gpa.csv" for s in schools]:
        os.remove(p)


def main():
    try:
        run()
    except BatchError as e:
        print("grade_close failed: %s" % e)


main()
