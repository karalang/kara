"""Mirror of cron_sched.kara. Calendar math comes from Python's datetime
(an independent oracle for the hand-written days-from-civil code); the cron
parsing and the catch-up logic are a port of the Kara."""
import datetime as dt
import subprocess

CRONTAB = """# m h dom mon dow  name  command
*/15 * * * *        tick     echo tick
0 9 * * mon-fri     standup  echo standup
30 2 1 * *          billing  echo billing run
0 0 29 2 *          leapday  echo leap day
0 12 13 * fri       friday13 echo spooky
@weekly             backup   sh -c 'echo backup; exit 3'
5,35 8-10/2 * jan,jul *  odd  echo odd hours
0 0 31 * *          monthend echo thirty-first
61 * * * *          broken   echo never
0 0 * * 9           badday   echo never
"""

UTC = dt.timezone.utc
EPOCH = dt.datetime(1970, 1, 1, tzinfo=UTC)
MONTHS = ["jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec"]
DAYS = ["sun", "mon", "tue", "wed", "thu", "fri", "sat"]


def civil(t):
    return EPOCH + dt.timedelta(seconds=t)


def epoch(d):
    return int((d - EPOCH).total_seconds())


def weekday(t):
    return (civil(t).weekday() + 1) % 7


def stamp(t):
    c = civil(t)
    return f"{c:%Y-%m-%d %H:%M} {['Sun', 'Mon', 'Tue', 'Wed', 'Thu', 'Fri', 'Sat'][weekday(t)]}"


class Bad(Exception):
    pass


def name_value(s, names, base):
    low = s.lower()
    if low in names:
        return names.index(low) + base
    try:
        return int(s)
    except ValueError:
        return None


def parse_field(f, lo, hi, names, base):
    vals = set()
    for part in f.split(","):
        if "/" in part:
            rng, st = part.split("/", 1)
            try:
                step = int(st)
            except ValueError:
                raise Bad(f"bad step in {part}")
            if step <= 0:
                raise Bad(f"step must be positive in {part}")
        else:
            rng, step = part, 1
        if rng == "*":
            a, b = lo, hi
        elif "-" in rng:
            x, y = rng.split("-", 1)
            a, b = name_value(x, names, base), name_value(y, names, base)
            if a is None or b is None:
                raise Bad(f"bad range {rng}")
        else:
            a = name_value(rng, names, base)
            if a is None:
                raise Bad(f"bad value {rng}")
            b = hi if step > 1 else a
        if a < lo or b > hi or a > b:
            raise Bad(f"{part} is outside {lo}-{hi}")
        vals.update(range(a, b + 1, step))
    return vals


class Spec:
    def __init__(self, f):
        self.minutes = parse_field(f[0], 0, 59, [], 0)
        self.hours = parse_field(f[1], 0, 23, [], 0)
        self.doms = parse_field(f[2], 1, 31, [], 0)
        self.months = parse_field(f[3], 1, 12, MONTHS, 1)
        dows = parse_field(f[4], 0, 7, DAYS, 0)
        if 7 in dows:
            dows = (dows | {0}) - {7}
        self.dows = dows
        self.dom_star = f[2] == "*"
        self.dow_star = f[4] == "*"


def day_matches(s, c, t):
    dom = c.day in s.doms
    dow = weekday(t) in s.dows
    return (dom and dow) if (s.dom_star or s.dow_star) else (dom or dow)


def next_fire(s, after, limit):
    t = (after // 60) * 60 + 60
    stop = after + limit * 366 * 86400
    while t <= stop:
        c = civil(t)
        if c.month not in s.months:
            y, m = (c.year + 1, 1) if c.month == 12 else (c.year, c.month + 1)
            t = epoch(dt.datetime(y, m, 1, tzinfo=UTC))
            continue
        if not day_matches(s, c, t):
            t = (t // 86400 + 1) * 86400
            continue
        if c.hour not in s.hours:
            t = (t // 3600) * 3600 + 3600
            continue
        if c.minute not in s.minutes:
            t += 60
            continue
        return t
    return None


MACROS = {"@hourly": "0 * * * *", "@daily": "0 0 * * *", "@midnight": "0 0 * * *",
          "@weekly": "0 0 * * 0", "@monthly": "0 0 1 * *", "@yearly": "0 0 1 1 *", "@annually": "0 0 1 1 *"}


def parse_crontab(text):
    jobs, errors = [], []
    for n, line in enumerate(text.splitlines(), 1):
        t = line.strip()
        if not t or t.startswith("#"):
            continue
        words = t.split()
        if words[0].startswith("@"):
            if words[0] not in MACROS:
                errors.append(f"line {n}: unknown macro {words[0]}")
                continue
            time, rest = MACROS[words[0]], 1
        else:
            if len(words) < 7:
                errors.append(f"line {n}: too few fields")
                continue
            time, rest = " ".join(words[:5]), 5
        if len(words) < rest + 2:
            errors.append(f"line {n}: no command")
            continue
        try:
            spec = Spec(time.split(" "))
        except Bad as e:
            errors.append(f"line {n} ({words[rest]}): {e}")
            continue
        jobs.append((words[rest], " ".join(words[rest + 1:]), spec, time))
    return jobs, errors


def run_command(cmd):
    r = subprocess.run(["sh", "-c", cmd], capture_output=True, text=True)
    return r.returncode, r.stdout.strip()


def load_state():
    m = {}
    try:
        with open("cron.state") as f:
            for line in f.read().splitlines():
                p = line.split(" ")
                if len(p) == 2:
                    try:
                        m[p[0]] = int(p[1])
                    except ValueError:
                        pass
    except FileNotFoundError:
        pass
    return m


def save_state(m):
    with open("cron.state", "w") as f:
        f.write("".join(f"{k} {m[k]}\n" for k in sorted(m)))


def start(jobs, now, first_start):
    print(f"== start at {stamp(now)}")
    state = load_state()
    for name, cmd, spec, _ in jobs:
        t = state.get(name, first_start)
        missed, last = 0, -1
        while True:
            f = next_fire(spec, t, 1)
            if f is None or f > now:
                break
            missed += 1
            last = f
            t = f
        if missed > 0:
            code, out = run_command(cmd)
            print(f"  ran {name} for {stamp(last)} ({missed} slot(s) due): exit {code} [{out}]")
            state[name] = now
    save_state(state)
    print("  next:")
    for name, _, spec, expr in jobs:
        f = next_fire(spec, now, 8)
        print(f"    {name:<9} {expr:<20} {stamp(f) if f is not None else 'never'}")


def main():
    for y, m, d in [(1970, 1, 1), (2000, 2, 29), (2024, 12, 31), (1969, 12, 31), (1900, 3, 1)]:
        days = (dt.date(y, m, d) - dt.date(1970, 1, 1)).days
        back = dt.date(1970, 1, 1) + dt.timedelta(days=days)
        ok = "ok" if (back.year, back.month, back.day) == (y, m, d) else "MISMATCH"
        print(f"{y}-{m:02d}-{d:02d} = day {days}, weekday {weekday(days * 86400)}, round trip {ok}")
    import calendar
    print(f"days in Feb 1900/2000/2024: {calendar.monthrange(1900, 2)[1]} {calendar.monthrange(2000, 2)[1]} "
          f"{calendar.monthrange(2024, 2)[1]}")
    with open("crontab", "w") as f:
        f.write(CRONTAB)
    with open("crontab") as f:
        jobs, errors = parse_crontab(f.read())
    print(f"crontab: {len(jobs)} jobs, {len(errors)} errors")
    for e in errors:
        print(f"  {e}")
    first = epoch(dt.datetime(2026, 1, 29, 23, 50, 0, tzinfo=UTC))
    now = epoch(dt.datetime(2026, 2, 2, 9, 20, 17, tzinfo=UTC))
    start(jobs, now, first)
    start(jobs, now + 3600 + 600, first)


main()
