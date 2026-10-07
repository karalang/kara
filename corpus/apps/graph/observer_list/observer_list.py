#!/usr/bin/env python3
"""Python mirror of source.orig.kara: a tiny spreadsheet with observers."""


class SheetError(Exception):
    def __init__(self, kind, *args):
        super().__init__(kind)
        self.kind = kind
        self.args_ = args

    def __str__(self):
        a = self.args_
        if self.kind == "UnknownCell":
            return f"unknown cell {a[0]}"
        if self.kind == "DuplicateCell":
            return f"cell {a[0]} already defined"
        if self.kind == "Cycle":
            return f"cycle: {a[0]} would depend on itself via {a[1]}"
        return f"division by zero in {a[0]}"


class Formula:
    def __init__(self, kind, *args):
        self.kind = kind
        self.args = args

    def deps(self):
        k, a = self.kind, self.args
        if k == "Const":
            return []
        if k in ("Sum", "Max"):
            return list(a[0])
        if k == "Scale":
            return [a[0]]
        return [a[0], a[1]]

    def __str__(self):
        k, a = self.kind, self.args
        if k == "Const":
            return f"{a[0]}"
        if k == "Sum":
            return f"SUM({', '.join(a[0])})"
        if k == "Max":
            return f"MAX({', '.join(a[0])})"
        if k == "Scale":
            return f"{a[0]} * {a[1]}"
        if k == "Product":
            return f"{a[0]} * {a[1]}"
        if k == "Diff":
            return f"{a[0]} - {a[1]}"
        return f"{a[0]} / {a[1]}"


class Cell:
    def __init__(self, name, formula, value):
        self.name = name
        self.formula = formula
        self.value = value


KEEP, UNSUB = "keep", "unsub"


class Observer:
    def __init__(self, oid, label, watch, kind, param=0):
        self.id = oid
        self.label = label
        self.watch = watch
        self.kind = kind
        self.param = param
        self.seen = 0

    def watches(self, cell):
        return self.watch == "*" or self.watch == cell

    def notify(self, cell, old, new):
        self.seen += 1
        tag = f"[{self.label}#{self.id}]"
        if self.kind == "Audit":
            print(f"    {tag} {cell}: {old} -> {new}")
            return KEEP
        if self.kind == "Threshold":
            limit = self.param
            if new > limit:
                print(f"    {tag} ALERT {cell}={new} exceeds {limit}; unsubscribing")
                return UNSUB
            print(f"    {tag} {cell}={new} within {limit}")
            return KEEP
        if self.kind == "OneShot":
            print(f"    {tag} first change of {cell} seen ({old} -> {new}); unsubscribing")
            return UNSUB
        if self.kind == "Countdown":
            left = self.param - self.seen
            if left <= 0:
                print(f"    {tag} {cell}={new}, last notification; unsubscribing")
                return UNSUB
            print(f"    {tag} {cell}={new}, {left} left")
            return KEEP
        delta = new - old
        direction = "up" if delta > 0 else "down"
        print(f"    {tag} {cell} went {direction} by {abs(delta)}")
        return KEEP


class Sheet:
    def __init__(self):
        self.cells = []
        self.index = {}
        self.observers = []
        self.next_id = 1

    def lookup(self, name):
        if name in self.index:
            return self.index[name]
        raise SheetError("UnknownCell", name)

    def value_of(self, name):
        return self.cells[self.lookup(name)].value

    def eval(self, name, f):
        k, a = f.kind, f.args
        if k == "Const":
            return a[0]
        if k == "Sum":
            total = 0
            for n in a[0]:
                total += self.value_of(n)
            return total
        if k == "Max":
            best = self.value_of(a[0][0])
            for n in a[0]:
                v = self.value_of(n)
                if v > best:
                    best = v
            return best
        if k == "Scale":
            return self.value_of(a[0]) * a[1]
        if k == "Product":
            return self.value_of(a[0]) * self.value_of(a[1])
        if k == "Diff":
            return self.value_of(a[0]) - self.value_of(a[1])
        d = self.value_of(a[1])
        if d == 0:
            raise SheetError("DivByZero", name)
        return int(self.value_of(a[0]) / d)  # truncating division, like i64

    def reaches(self, frm, target):
        stack = [frm]
        visited = set()
        while stack:
            cur = stack.pop()
            if cur == target:
                return True
            if cur in visited:
                continue
            visited.add(cur)
            if cur in self.index:
                for d in self.cells[self.index[cur]].formula.deps():
                    stack.append(d)
        return False

    def check_formula(self, name, f):
        for d in f.deps():
            self.lookup(d)
            if self.reaches(d, name):
                raise SheetError("Cycle", name, d)

    def define(self, name, f):
        if name in self.index:
            raise SheetError("DuplicateCell", name)
        self.check_formula(name, f)
        value = self.eval(name, f)
        print(f"define {name} = {f} -> {value}")
        self.index[name] = len(self.cells)
        self.cells.append(Cell(name, f, value))

    def topo_order(self):
        n = len(self.cells)
        placed = [False] * n
        order = []
        while len(order) < n:
            for i in range(n):
                if not placed[i]:
                    ready = all(placed[self.index[d]] for d in self.cells[i].formula.deps())
                    if ready:
                        placed[i] = True
                        order.append(i)
                        break
        return order

    def affected_by(self, start):
        hit = [False] * len(self.cells)
        hit[start] = True
        queue = [start]
        while queue:
            cur = queue.pop()
            cur_name = self.cells[cur].name
            for j in range(len(self.cells)):
                if not hit[j] and cur_name in self.cells[j].formula.deps():
                    hit[j] = True
                    queue.append(j)
        return hit

    def set(self, name, f):
        idx = self.lookup(name)
        self.check_formula(name, f)
        print(f"set {name} = {f}")
        self.cells[idx].formula = f
        hit = self.affected_by(idx)
        changes = []
        for i in self.topo_order():
            if not hit[i]:
                continue
            old = self.cells[i].value
            cell_name = self.cells[i].name
            try:
                v = self.eval(cell_name, self.cells[i].formula)
                if v != old:
                    self.cells[i].value = v
                    changes.append((i, old, v))
            except SheetError as e:
                print(f"  ! {e}; {cell_name} keeps {old}")
        print(f"  recomputed, {len(changes)} cell(s) changed")
        for (ci, old, new) in changes:
            self.notify_all(self.cells[ci].name, old, new)
        return len(changes)

    def notify_all(self, cell, old, new):
        i = 0
        while i < len(self.observers):
            if not self.observers[i].watches(cell):
                i += 1
                continue
            if self.observers[i].notify(cell, old, new) == KEEP:
                i += 1
            else:
                gone = self.observers.pop(i)
                print(f"    - {gone.label}#{gone.id} removed ({gone.seen} notification(s))")

    def subscribe(self, label, watch, kind, param=0):
        oid = self.next_id
        self.next_id += 1
        print(f"subscribe {label}#{oid} on {watch}")
        self.observers.append(Observer(oid, label, watch, kind, param))
        return oid

    def unsubscribe(self, oid):
        for i, o in enumerate(self.observers):
            if o.id == oid:
                self.observers.pop(i)
                print(f"unsubscribe {o.label}#{o.id}")
                return True
        return False

    def report(self):
        print("final values:")
        for c in self.cells:
            print(f"  {c.name} = {c.value}    ({c.formula})")
        print(f"subscribers left: {len(self.observers)}")
        for o in self.observers:
            print(f"  {o.label}#{o.id} on {o.watch}, notified {o.seen} time(s)")


def report_err(step, fn):
    try:
        fn()
    except SheetError as e:
        print(f"{step} failed: {e}")


def apply(sheet, name, f):
    try:
        sheet.set(name, f)
    except SheetError as e:
        print(f"set {name} rejected: {e}")


def build_sheet(sheet):
    sheet.define("A1", Formula("Const", 10))
    sheet.define("A2", Formula("Const", 20))
    sheet.define("A3", Formula("Sum", ["A1", "A2"]))
    sheet.define("B1", Formula("Scale", "A3", 2))
    sheet.define("B2", Formula("Product", "A1", "A2"))
    sheet.define("C1", Formula("Max", ["B1", "B2", "A3"]))
    sheet.define("C2", Formula("Diff", "C1", "B1"))
    sheet.define("D1", Formula("Div", "C1", "A1"))


def main():
    sheet = Sheet()
    report_err("build", lambda: build_sheet(sheet))
    report_err("redefine", lambda: sheet.define("B1", Formula("Const", 1)))
    report_err("define", lambda: sheet.define("E1", Formula("Sum", ["A1", "Z9"])))

    sheet.subscribe("audit", "*", "Audit")
    sheet.subscribe("alarm", "C1", "Threshold", 300)
    sheet.subscribe("once", "A3", "OneShot")
    sheet.subscribe("tick", "B1", "Countdown", 2)
    sheet.subscribe("b1-once", "B1", "OneShot")
    trend = sheet.subscribe("trend", "C2", "Trend")

    apply(sheet, "A1", Formula("Const", 15))
    apply(sheet, "A2", Formula("Const", 5))
    apply(sheet, "A1", Formula("Const", 0))
    apply(sheet, "A1", Formula("Sum", ["C2", "A2"]))
    apply(sheet, "Z9", Formula("Const", 3))
    apply(sheet, "A2", Formula("Const", 100))
    apply(sheet, "A3", Formula("Const", 7))
    if sheet.unsubscribe(trend):
        print("trend observer detached")
    if not sheet.unsubscribe(trend):
        print(f"observer #{trend} was already gone")
    apply(sheet, "A1", Formula("Const", 4))
    sheet.report()


if __name__ == "__main__":
    main()
