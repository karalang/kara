"""Python mirror of source.orig.kara: same data, same greedy resolver, same output."""


class ParseError(Exception):
    pass


class ResolveError(Exception):
    def __init__(self, kind, **fields):
        super().__init__(kind)
        self.kind = kind
        self.fields = fields

    def describe(self):
        f = self.fields
        if self.kind == "parse":
            return f"parse error: {f['msg']}"
        if self.kind == "missing":
            return f"package '{f['name']}' not found (required by {f['required_by']})"
        if self.kind == "nomatch":
            return (f"no version of '{f['name']}' matches {f['wanted']} "
                    f"(required by {f['required_by']}; available: {f['available']})")
        if self.kind == "conflict":
            return (f"conflict on '{f['name']}': {f['chosen']} already selected, "
                    f"but {f['required_by']} needs {f['wanted']}")
        return "dependency cycle: " + " -> ".join(f["path"])


class Version:
    def __init__(self, major, minor, patch):
        self.major, self.minor, self.patch = major, minor, patch

    @staticmethod
    def parse(text):
        parts = text.split(".")
        if len(parts) != 3:
            raise ParseError(f"bad version '{text}'")
        nums = []
        for p in parts:
            try:
                nums.append(int(p.strip()))
            except ValueError:
                raise ParseError(f"bad version '{text}'")
        return Version(nums[0], nums[1], nums[2])

    def compare(self, other):
        for a, b in ((self.major, other.major), (self.minor, other.minor), (self.patch, other.patch)):
            if a != b:
                return -1 if a < b else 1
        return 0

    def text(self):
        return f"{self.major}.{self.minor}.{self.patch}"


class Constraint:
    def __init__(self, kind, version=None):
        self.kind, self.version = kind, version

    @staticmethod
    def parse(text):
        t = text.strip()
        if t == "*":
            return Constraint("any")
        if t.startswith(">="):
            return Constraint("atleast", Version.parse(t[2:]))
        if t.startswith("^"):
            return Constraint("caret", Version.parse(t[1:]))
        if t.startswith("="):
            return Constraint("exact", Version.parse(t[1:]))
        raise ParseError(f"bad constraint '{t}'")

    def matches(self, v):
        if self.kind == "any":
            return True
        base = self.version
        if self.kind == "exact":
            return v.compare(base) == 0
        if self.kind == "atleast":
            return v.compare(base) >= 0
        if v.compare(base) < 0:
            return False
        if base.major > 0:
            return v.major == base.major
        return v.major == 0 and v.minor == base.minor

    def text(self):
        if self.kind == "any":
            return "*"
        prefix = {"exact": "=", "atleast": ">=", "caret": "^"}[self.kind]
        return prefix + self.version.text()


class Dep:
    def __init__(self, name, constraint):
        self.name, self.constraint = name, constraint


class Release:
    def __init__(self, name, version, deps):
        self.name, self.version, self.deps = name, version, deps


def parse_dep(text):
    t = text.strip()
    pieces = t.split(" ")
    if len(pieces) != 2:
        raise ParseError(f"bad dependency '{t}'")
    return Dep(pieces[0], Constraint.parse(pieces[1]))


def parse_release(line):
    halves = line.split(":")
    head = halves[0].strip().split(" ")
    if len(head) != 2:
        raise ParseError(f"bad release header '{line}'")
    version = Version.parse(head[1])
    deps = []
    if len(halves) > 1:
        for item in halves[1].split(","):
            if item.strip():
                deps.append(parse_dep(item))
    return Release(head[0], version, deps)


class Resolver:
    def __init__(self, releases):
        self.releases = releases
        self.chosen = {}

    @staticmethod
    def load(text):
        releases = []
        for line in text.split("\n"):
            t = line.strip()
            if not t or t.startswith("#"):
                continue
            releases.append(parse_release(t))
        return Resolver(releases)

    def best_match(self, name, c, required_by):
        best = -1
        available = []
        for i, rel in enumerate(self.releases):
            if rel.name == name:
                available.append(rel.version.text())
                if c.matches(rel.version):
                    if best < 0 or rel.version.compare(self.releases[best].version) > 0:
                        best = i
        if not available:
            raise ResolveError("missing", name=name, required_by=required_by)
        if best < 0:
            raise ResolveError("nomatch", name=name, wanted=c.text(), required_by=required_by,
                               available=", ".join(available))
        return best

    def require(self, name, c, required_by):
        if name in self.chosen:
            rel = self.releases[self.chosen[name]]
            if c.matches(rel.version):
                return
            raise ResolveError("conflict", name=name, chosen=rel.version.text(),
                               wanted=c.text(), required_by=required_by)
        idx = self.best_match(name, c, required_by)
        self.chosen[name] = idx
        rel = self.releases[idx]
        who = rel.name + "@" + rel.version.text()
        for dep in rel.deps:
            self.require(dep.name, dep.constraint, who)

    def visit(self, name, state, stack, order):
        mark = state.get(name, 0)
        if mark == 2:
            return
        if mark == 1:
            start = stack.index(name)
            raise ResolveError("cycle", path=stack[start:] + [name])
        state[name] = 1
        stack.append(name)
        rel = self.releases[self.chosen[name]]
        for dep in rel.deps:
            self.visit(dep.name, state, stack, order)
        stack.pop()
        state[name] = 2
        order.append(name)

    def plan(self, roots):
        for root in roots:
            self.require(root.name, root.constraint, "<root>")
        state, stack, order = {}, [], []
        for root in roots:
            self.visit(root.name, state, stack, order)
        return order


def run_scenario(title, file, roots_text):
    try:
        with open(file) as fh:
            text = fh.read()
    except OSError:
        raise ResolveError("parse", msg=f"cannot read {file}")
    try:
        resolver = Resolver.load(text)
    except ParseError as e:
        raise ResolveError("parse", msg=str(e))
    roots = []
    for item in roots_text.split(","):
        try:
            roots.append(parse_dep(item))
        except ParseError as e:
            raise ResolveError("parse", msg=str(e))
    order = resolver.plan(roots)
    lines = []
    for name in order:
        rel = resolver.releases[resolver.chosen[name]]
        line = f"{name} {rel.version.text()}"
        if rel.deps:
            line = line + " (needs " + ", ".join(d.name for d in rel.deps) + ")"
        lines.append(line)
    return lines


def main():
    base = "log 0.3.0\nlog 0.3.2\nlog 1.0.0\njson 1.1.0: log ^0.3.0\njson 1.4.0: log ^0.3.1\n"
    web = base + "http 2.0.0: json ^1.2.0, log >=0.3.0\nhttp 2.1.0: json ^1.2.0, log ^0.3.0\n# app\napp 1.0.0: http ^2.0.0, json ^1.0.0\n"
    cyclic = base + "app 1.0.0: core ^1.0.0\ncore 1.0.0: plugin ^1.0.0, log ^0.3.0\nplugin 1.0.0: hooks ^1.0.0\nhooks 1.2.0: core >=1.0.0\n"
    conflict = base + "http 2.0.0: json ^1.2.0\napp 1.0.0: http ^2.0.0, json =1.1.0\n"
    broken = base + "cli 0.9.0: args ^0.2.0, log >=2.0.0\nargs 0.2.5\n"

    for path, content in (("reg_web.txt", web), ("reg_cyclic.txt", cyclic),
                          ("reg_conflict.txt", conflict), ("reg_broken.txt", broken)):
        with open(path, "w") as fh:
            fh.write(content)

    scenarios = [
        ("web app", "reg_web.txt", "app ^1.0.0"),
        ("two roots sharing deps", "reg_web.txt", "json ^1.0.0, http >=2.0.0, log *"),
        ("plugin cycle", "reg_cyclic.txt", "app ^1.0.0"),
        ("version conflict", "reg_conflict.txt", "app ^1.0.0"),
        ("unsatisfiable", "reg_broken.txt", "cli ^0.9.0"),
        ("missing package", "reg_web.txt", "app ^1.0.0, metrics ^0.1.0"),
        ("pinned old json", "reg_web.txt", "json =1.1.0, log *"),
        ("pinned old log", "reg_web.txt", "log =0.3.0, json *"),
        ("bad manifest", "reg_web.txt", "app 1.x"),
    ]

    ok_count = failed = packages = 0
    for title, file, roots in scenarios:
        print(f"== {title} [{roots}] ==")
        try:
            lines = run_scenario(title, file, roots)
        except ResolveError as e:
            print(f"error: {e.describe()}")
            failed += 1
            continue
        print(f"plan: {len(lines)} packages")
        for step, line in enumerate(lines, 1):
            print(f"  {step}. install {line}")
        ok_count += 1
        packages += len(lines)
    print(f"summary: {ok_count} resolved ({packages} installs), {failed} failed")


if __name__ == "__main__":
    main()
