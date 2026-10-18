# The mirror of mini_systemd: the same unit loading and systemctl jobs
# in Python. expected.out is its output.
import os, shutil
import mk

ADMIN = "data/etc/systemd/system/"
VENDOR = "data/usr/lib/systemd/system/"
LISTS = {"Requires", "Wants", "After", "Before", "Conflicts", "WantedBy", "Environment"}
NO_INSTALL = ("The unit files have no installation config (WantedBy=, RequiredBy=, Also=,\n"
              "Alias= settings in the [Install] section, and DefaultInstance= for template\n"
              "units). This means they are not meant to be enabled or disabled using systemctl.")


def shown(path):
    return path[len("data"):]


def exists(p):
    return os.path.isfile(p)


def listdir(d):
    return sorted(os.listdir(d)) if os.path.isdir(d) else []


class Unit:
    def __init__(self, name):
        self.name = name
        self.found = False
        self.fragment = None
        self.dropins = []
        self.props = {}
        self.has_install = False
        self.template = None
        self.instance = ""

    def get(self, k):
        return self.props.get(k, [] if k in LISTS else "")

    def desc(self):
        return self.get("Description") or self.name

    def kind(self):
        return self.name.rsplit(".", 1)[1]


def split_name(name):
    """(template file name, instance) for an instance name, else (None, '')."""
    stem, suffix = name.rsplit(".", 1)
    if "@" in stem:
        p, i = stem.split("@", 1)
        if i:
            return "%s@.%s" % (p, suffix), i
    return None, ""


def specifiers(s, u):
    stem = u.name.rsplit(".", 1)[0]
    prefix = stem.split("@", 1)[0]
    out = ""
    i = 0
    while i < len(s):
        if s[i] == "%" and i + 1 < len(s):
            c = s[i + 1]
            out += {"n": u.name, "N": stem, "p": prefix, "i": u.instance, "%": "%"}.get(c, "%" + c)
            i += 2
        else:
            out += s[i]
            i += 1
    return out


def parse_into(u, text):
    section = ""
    for raw in text.split("\n"):
        line = raw.strip()
        if not line or line[0] in "#;":
            continue
        if line.startswith("[") and line.endswith("]"):
            section = line[1:-1]
            if section == "Install":
                u.has_install = True
            continue
        if "=" not in line:
            continue
        k, v = line.split("=", 1)
        k, v = k.strip(), v.strip()
        if k in LISTS:
            if v == "":
                u.props[k] = []
            else:
                u.props.setdefault(k, []).extend(v.split() if k != "Environment" else [v])
        else:
            u.props[k] = v


def load(name):
    u = Unit(name)
    template, inst = split_name(name)
    u.template = template
    u.instance = inst
    for d in (ADMIN, VENDOR):
        if exists(d + name):
            u.fragment = d + name
            break
    if u.fragment is None and template:
        for d in (ADMIN, VENDOR):
            if exists(d + template):
                u.fragment = d + template
                break
    if u.fragment is None:
        return u
    u.found = True
    parse_into(u, open(u.fragment).read())
    confs = {}
    names = [name] + ([template] if template else [])
    for d in (VENDOR, ADMIN):
        for n in names[::-1]:
            for f in listdir(d + n + ".d"):
                if f.endswith(".conf"):
                    confs[f] = d + n + ".d/" + f
    for f in sorted(confs):
        u.dropins.append(confs[f])
        parse_into(u, open(confs[f]).read())
    for f in listdir(ADMIN + name + ".wants"):
        u.props.setdefault("Wants", []).append(f)
    u.props["Description"] = specifiers(u.get("Description"), u)
    if "ExecStart" in u.props:
        u.props["ExecStart"] = specifiers(u.props["ExecStart"], u)
    return u


def links_to(name):
    out = []
    for d in listdir(ADMIN):
        if d.endswith(".wants") and exists(ADMIN + d + "/" + name):
            out.append(ADMIN + d + "/" + name)
    return out


class System:
    def __init__(self):
        self.state = {}  # name -> 'active' | 'failed'

    def active(self, n):
        return self.state.get(n) == "active"


def unit_file_state(u):
    if links_to(u.name):
        return "enabled"
    if not u.has_install:
        return "static"
    return "disabled"


def cmd_cat(sysm, name):
    u = load(name)
    if not u.found:
        print("No files found for %s." % name)
        return
    parts = [u.fragment] + u.dropins
    for i, p in enumerate(parts):
        if i:
            print()
        print("# " + shown(p))
        print(open(p).read().rstrip("\n"))


def cmd_show(sysm, name):
    u = load(name)
    print("Id=" + name)
    if not u.found:
        print("LoadState=not-found")
        print("ActiveState=inactive")
        return
    print("Description=" + u.desc())
    print("LoadState=loaded")
    print("FragmentPath=" + shown(u.fragment))
    print("DropInPaths=" + " ".join(shown(p) for p in u.dropins))
    for k in ("Requires", "Wants", "Conflicts", "Before", "After"):
        print("%s=%s" % (k, " ".join(u.get(k))))
    if u.kind() == "service":
        print("ExecStart=" + u.get("ExecStart"))
        print("User=" + (u.get("User") or "root"))
        print("Environment=" + " ".join(u.get("Environment")))
    print("ActiveState=" + sysm.state.get(name, "inactive"))
    print("UnitFileState=" + unit_file_state(u))


def cmd_enable(sysm, name):
    u = load(name)
    if not u.found:
        print("Failed to enable unit: Unit file %s does not exist." % name)
        return
    link = name
    if u.template and False:
        pass
    stem = name.rsplit(".", 1)[0]
    if stem.endswith("@"):
        di = u.get("DefaultInstance")
        if not di:
            print(NO_INSTALL)
            return
        link = "%s%s.%s" % (stem, di, name.rsplit(".", 1)[1])
    targets = u.get("WantedBy")
    if not targets:
        print(NO_INSTALL)
        return
    for t in targets:
        path = ADMIN + t + ".wants/" + link
        if exists(path):
            continue
        os.makedirs(ADMIN + t + ".wants", exist_ok=True)
        with open(path, "w") as f:
            f.write(shown(u.fragment) + "\n")
        print("Created symlink %s → %s." % (shown(path), shown(u.fragment)))


def cmd_disable(sysm, name):
    for p in links_to(name):
        os.remove(p)
        print('Removed "%s".' % shown(p))


def deps_of(u):
    return sorted(set(u.get("Requires")) | set(u.get("Wants")))


def cmd_list_deps(sysm, name):
    print(name)

    def walk(n, prefix, path):
        kids = deps_of(load(n))
        for i, k in enumerate(kids):
            last = i == len(kids) - 1
            dot = "●" if sysm.active(k) else "○"
            print("%s %s%s%s" % (dot, prefix, "└─" if last else "├─", k))
            if k.endswith(".target") and k not in path:
                walk(k, prefix + ("  " if last else "│ "), path | {k})

    walk(name, "", {name})


def journal(msg):
    print("  " + msg)


def conflicts_of(name, jobs_units):
    u = load(name)
    out = set(u.get("Conflicts"))
    for other, st in list(jobs_units.items()):
        if name in load(other).get("Conflicts"):
            out.add(other)
    return out


def cmd_start(sysm, name):
    print("$ systemctl start " + name)
    root = load(name)
    if not root.found:
        print("Failed to start %s: Unit %s not found." % (name, name))
        return
    units = {name: root}
    required = {name}
    queue = [name]
    while queue:
        n = queue.pop(0)
        u = units[n]
        for kind in ("Requires", "Wants"):
            for d in u.get(kind):
                if d in units:
                    if kind == "Requires" and n in required:
                        required.add(d)
                    continue
                du = load(d)
                if not du.found:
                    if kind == "Requires" and n in required:
                        print("Failed to start %s: Unit %s not found." % (name, d))
                        return
                    continue
                units[d] = du
                if kind == "Requires" and n in required:
                    required.add(d)
                queue.append(d)
    jobs = sorted(n for n in units if not sysm.active(n))
    stops = set()
    for n in units:
        for c in conflicts_of(n, sysm.state):
            if sysm.active(c):
                stops.add(c)

    def before(n):
        """Jobs that must run before n."""
        u = units[n]
        out = set(d for d in u.get("After") if d in jobs)
        for m in jobs:
            if n in units[m].get("Before"):
                out.add(m)
        out.discard(n)
        return sorted(out)

    while True:
        cycle = None
        color = {}
        stack = []

        def dfs(n):
            nonlocal cycle
            color[n] = 1
            stack.append(n)
            for d in before(n):
                if cycle:
                    return
                if color.get(d) == 1:
                    cycle = stack[stack.index(d):]
                    return
                if d not in color:
                    dfs(d)
            stack.pop()
            color[n] = 2

        for n in jobs:
            if n not in color and not cycle:
                dfs(n)
        if not cycle:
            break
        journal("Found ordering cycle on %s/start" % cycle[0])
        for c in cycle[1:]:
            journal("Found dependency on %s/start" % c)
        journal("Found dependency on %s/start" % cycle[0])
        victim = None
        for c in cycle:
            if c not in required:
                victim = c
                break
        if victim is None:
            journal("Unable to break cycle starting with %s/start" % cycle[0])
            print("Failed to start %s: Transaction order is cyclic. See system logs for details." % name)
            return
        journal("Job %s/start deleted to break ordering cycle starting with %s/start" % (victim, cycle[0]))
        jobs.remove(victim)

    for s in sorted(stops):
        su = load(s)
        if su.kind() == "target":
            journal("Stopped target %s." % su.desc())
        else:
            journal("Stopping %s..." % su.desc())
            journal("Stopped %s." % su.desc())
        del sysm.state[s]

    order = []
    done = set()
    while len(order) < len(jobs):
        ready = [n for n in jobs if n not in done and all(d in done for d in before(n))]
        n = sorted(ready)[0]
        order.append(n)
        done.add(n)
    result = {}
    for n in order:
        u = units[n]
        failed_dep = [d for d in u.get("Requires") if result.get(d) in ("failed", "dependency") and d in u.get("After")]
        if failed_dep:
            journal("Dependency failed for %s." % u.desc())
            result[n] = "dependency"
            continue
        if u.kind() == "target":
            journal("Reached target %s." % u.desc())
            sysm.state[n] = "active"
            result[n] = "done"
            continue
        journal("Starting %s..." % u.desc())
        if "--fail" in u.get("ExecStart").split():
            journal("%s: Main process exited, code=exited, status=1/FAILURE" % n)
            journal("%s: Failed with result 'exit-code'." % n)
            journal("Failed to start %s." % u.desc())
            sysm.state[n] = "failed"
            result[n] = "failed"
            continue
        if u.get("Type") == "oneshot":
            journal("Finished %s." % u.desc())
        else:
            journal("Started %s." % u.desc())
        sysm.state[n] = "active"
        result[n] = "done"
    r = result.get(name, "done")
    if r == "dependency":
        print("A dependency job for %s failed. See 'journalctl -xe' for details." % name)
    elif r == "failed":
        print("Job for %s failed because the control process exited with error code." % name)


def cmd_stop(sysm, name):
    print("$ systemctl stop " + name)
    stop = {name}
    changed = True
    while changed:
        changed = False
        for n in sorted(sysm.state):
            if sysm.active(n) and n not in stop and stop & set(load(n).get("Requires")):
                stop.add(n)
                changed = True
    stop = sorted(n for n in stop if sysm.active(n))
    done = []
    while len(done) < len(stop):
        # A unit stops after every unit ordered after it.
        ready = [n for n in stop if n not in done and
                 all(m in done for m in stop if m != n and n in load(m).get("After"))]
        n = sorted(ready)[0]
        done.append(n)
        u = load(n)
        if u.kind() == "target":
            journal("Stopped target %s." % u.desc())
        else:
            journal("Stopping %s..." % u.desc())
            journal("Stopped %s." % u.desc())
        del sysm.state[n]


def cmd_list_units(sysm):
    names = sorted(sysm.state)
    w = max([len("UNIT")] + [len(n) for n in names])
    print("%-*s LOAD   ACTIVE SUB     DESCRIPTION" % (w, "UNIT"))
    for n in names:
        u = load(n)
        st = sysm.state[n]
        if st == "failed":
            sub = "failed"
        elif u.kind() == "target":
            sub = "active"
        elif u.get("Type") == "oneshot":
            sub = "exited"
        else:
            sub = "running"
        print("%-*s loaded %-6s %-7s %s" % (w, n, st, sub, u.desc()))
    print("%d loaded units listed." % len(names))


def main():
    shutil.rmtree("data", ignore_errors=True)
    mk.write_all()
    sysm = System()
    for line in open("data/jobs.txt").read().split("\n"):
        if not line or line.startswith("#"):
            continue
        parts = line.split()
        cmd, arg = parts[0], (parts[1] if len(parts) > 1 else "")
        if cmd not in ("start", "stop"):
            print("$ systemctl " + line)
        if cmd == "cat":
            cmd_cat(sysm, arg)
        elif cmd == "show":
            cmd_show(sysm, arg)
        elif cmd == "enable":
            cmd_enable(sysm, arg)
        elif cmd == "disable":
            cmd_disable(sysm, arg)
        elif cmd == "list-dependencies":
            cmd_list_deps(sysm, arg)
        elif cmd == "start":
            cmd_start(sysm, arg)
        elif cmd == "stop":
            cmd_stop(sysm, arg)
        elif cmd == "list-units":
            cmd_list_units(sysm)
        print()


main()
