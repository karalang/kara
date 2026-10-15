# Python mirror of csv_query.kara: the same CSV loader, tokenizer,
# recursive-descent parser, evaluator and table printer, written by hand
# (no csv or sqlite3 module). Values are None, int or str; booleans are the
# ints 1 and 0, as in the Kara program.


class QueryError(Exception):
    pass


def show_val(v):
    if v is None:
        return "NULL"
    return str(v)


def rank(v):
    return 0 if v is None else (1 if isinstance(v, int) else 2)


def compare(a, b):
    ra, rb = rank(a), rank(b)
    if ra != rb:
        return -1 if ra < rb else 1
    if a is None:
        return 0
    return -1 if a < b else (1 if a > b else 0)


def truthy(v):
    return isinstance(v, int) and v != 0


def bool_val(b):
    return 1 if b else 0


def parse_int(s):
    body = s[1:] if s[:1] in "+-" else s
    if not body or not all(c in "0123456789" for c in body):
        return None
    return int(s)


# ---- CSV -----------------------------------------------------------------

class Table:
    def __init__(self, name, cols, rows, types):
        self.name, self.cols, self.rows, self.types = name, cols, rows, types


def csv_fields(line, lineno):
    out, cur = [], ""
    i = 0
    quoted = False
    while i < len(line):
        c = line[i]
        if quoted:
            if c == '"':
                if i + 1 < len(line) and line[i + 1] == '"':
                    cur += '"'
                    i += 2
                    continue
                quoted = False
            else:
                cur += c
        elif c == '"':
            if cur:
                raise QueryError(f"line {lineno}: quote inside an unquoted field")
            quoted = True
        elif c == ",":
            out.append(cur)
            cur = ""
        else:
            cur += c
        i += 1
    if quoted:
        raise QueryError(f"line {lineno}: unterminated quoted field")
    out.append(cur)
    return out


def load_csv(name, text):
    lines = text.split("\n")
    if lines and lines[-1] == "":
        lines.pop()
    if not lines:
        raise QueryError(f"{name}.csv: empty file")
    try:
        cols = csv_fields(lines[0], 1)
    except QueryError as e:
        raise QueryError(f"{name}.csv {e}")
    raw = []
    for i in range(1, len(lines)):
        if not lines[i]:
            continue
        try:
            fields = csv_fields(lines[i], i + 1)
        except QueryError as e:
            raise QueryError(f"{name}.csv {e}")
        if len(fields) != len(cols):
            raise QueryError(f"{name}.csv line {i + 1}: expected {len(cols)} fields, found {len(fields)}")
        raw.append(fields)
    types = []
    for c in range(len(cols)):
        all_int = all(not r[c] or parse_int(r[c]) is not None for r in raw)
        types.append("int" if all_int else "text")
    rows = []
    for r in raw:
        row = []
        for c in range(len(cols)):
            if not r[c]:
                row.append(None)
            elif types[c] == "int":
                row.append(parse_int(r[c]))
            else:
                row.append(r[c])
        rows.append(row)
    return Table(name, cols, rows, types)


# ---- tokens and syntax ---------------------------------------------------

KEYWORDS = {"SELECT", "DISTINCT", "FROM", "JOIN", "LEFT", "ON", "WHERE", "GROUP", "BY",
            "HAVING", "ORDER", "ASC", "DESC", "LIMIT", "OFFSET", "AS", "AND", "OR", "NOT",
            "IS", "NULL", "LIKE", "IN"}


def tokenize(src):
    toks, pos = [], []
    i = 0
    while i < len(src):
        c = src[i]
        if c == " ":
            i += 1
            continue
        pos.append(i)
        if c.isalpha() or c == "_":
            w = ""
            while i < len(src) and (src[i].isalnum() or src[i] == "_"):
                w += src[i]
                i += 1
            toks.append(("ident", w))
        elif c.isdigit():
            n = 0
            while i < len(src) and src[i].isdigit():
                n = n * 10 + int(src[i])
                i += 1
            toks.append(("num", n))
        elif c == "'":
            s = ""
            i += 1
            while True:
                if i >= len(src):
                    raise QueryError(f"syntax error at offset {pos[-1]}: unterminated string")
                if src[i] == "'":
                    if i + 1 < len(src) and src[i + 1] == "'":
                        s += "'"
                        i += 2
                        continue
                    i += 1
                    break
                s += src[i]
                i += 1
            toks.append(("str", s))
        else:
            two = src[i:i + 2]
            if two in ("<=", ">=", "!=", "<>"):
                toks.append(("sym", "!=" if two == "<>" else two))
                i += 2
            elif c in "(),.*+-/%=<>":
                toks.append(("sym", c))
                i += 1
            else:
                raise QueryError(f"syntax error at offset {i}: unexpected '{c}'")
    pos.append(len(src))
    toks.append(("end", None))
    return toks, pos


# Expressions are tuples: ("lit", v), ("col", q, c), ("star",),
# ("bin", op, a, b), ("not", a), ("neg", a), ("isnull", a, neg),
# ("like", a, p, neg), ("in", a, list, neg), ("call", f, args).

STAR = ("star",)


class Parser:
    def __init__(self, toks, pos):
        self.toks, self.pos, self.at = toks, pos, 0

    def fail(self, what):
        here = "end of query" if self.toks[self.at][0] == "end" else f"offset {self.pos[self.at]}"
        return QueryError(f"syntax error at {here}: {what}")

    def is_kw(self, kw):
        k, v = self.toks[self.at]
        return k == "ident" and v.upper() == kw

    def eat_kw(self, kw):
        if self.is_kw(kw):
            self.at += 1
            return True
        return False

    def expect_kw(self, kw):
        if not self.eat_kw(kw):
            raise self.fail(f"expected {kw}")

    def is_sym(self, s):
        k, v = self.toks[self.at]
        return k == "sym" and v == s

    def eat_sym(self, s):
        if self.is_sym(s):
            self.at += 1
            return True
        return False

    def expect_sym(self, s):
        if not self.eat_sym(s):
            raise self.fail(f"expected '{s}'")

    def name(self, what):
        k, v = self.toks[self.at]
        if k != "ident":
            raise self.fail(f"expected {what}")
        if v.upper() in KEYWORDS:
            raise self.fail(f"expected {what}, found keyword {v.upper()}")
        self.at += 1
        return v

    def optional_alias(self):
        if self.eat_kw("AS"):
            return self.name("an alias")
        k, v = self.toks[self.at]
        if k == "ident" and v.upper() not in KEYWORDS:
            self.at += 1
            return v
        return ""

    def query(self):
        self.expect_kw("SELECT")
        q = {"distinct": self.eat_kw("DISTINCT"), "items": []}
        while True:
            if self.eat_sym("*"):
                q["items"].append((STAR, ""))
            else:
                e = self.expr()
                q["items"].append((e, self.optional_alias()))
            if not self.eat_sym(","):
                break
        self.expect_kw("FROM")
        q["from"] = self.name("a table name")
        q["from_alias"] = self.optional_alias()
        q["joins"] = []
        while True:
            left = self.eat_kw("LEFT")
            if not self.eat_kw("JOIN"):
                if left:
                    raise self.fail("expected JOIN")
                break
            table = self.name("a table name")
            alias = self.optional_alias()
            self.expect_kw("ON")
            q["joins"].append((table, alias, self.expr(), left))
        q["filter"] = self.expr() if self.eat_kw("WHERE") else None
        q["group"] = []
        if self.eat_kw("GROUP"):
            self.expect_kw("BY")
            while True:
                q["group"].append(self.expr())
                if not self.eat_sym(","):
                    break
        q["having"] = self.expr() if self.eat_kw("HAVING") else None
        q["order"] = []
        if self.eat_kw("ORDER"):
            self.expect_kw("BY")
            while True:
                e = self.expr()
                if self.eat_kw("DESC"):
                    desc = True
                else:
                    self.eat_kw("ASC")
                    desc = False
                q["order"].append((e, desc))
                if not self.eat_sym(","):
                    break
        q["limit"], q["offset"] = None, 0
        if self.eat_kw("LIMIT"):
            q["limit"] = self.number()
            if self.eat_kw("OFFSET"):
                q["offset"] = self.number()
        if self.toks[self.at][0] != "end":
            raise self.fail("unexpected text after the query")
        return q

    def number(self):
        k, v = self.toks[self.at]
        if k != "num":
            raise self.fail("expected a number")
        self.at += 1
        return v

    def expr(self):
        e = self.and_expr()
        while self.eat_kw("OR"):
            e = ("bin", "OR", e, self.and_expr())
        return e

    def and_expr(self):
        e = self.not_expr()
        while self.eat_kw("AND"):
            e = ("bin", "AND", e, self.not_expr())
        return e

    def not_expr(self):
        if self.eat_kw("NOT"):
            return ("not", self.not_expr())
        return self.comparison()

    def comparison(self):
        e = self.additive()
        if self.eat_kw("IS"):
            neg = self.eat_kw("NOT")
            self.expect_kw("NULL")
            return ("isnull", e, neg)
        neg = self.eat_kw("NOT")
        if self.eat_kw("LIKE"):
            return ("like", e, self.additive(), neg)
        if self.eat_kw("IN"):
            self.expect_sym("(")
            items = []
            while True:
                items.append(self.expr())
                if not self.eat_sym(","):
                    break
            self.expect_sym(")")
            return ("in", e, items, neg)
        if neg:
            raise self.fail("expected LIKE or IN after NOT")
        for op in ("=", "!=", "<=", ">=", "<", ">"):
            if self.eat_sym(op):
                return ("bin", op, e, self.additive())
        return e

    def additive(self):
        e = self.term()
        while True:
            if self.eat_sym("+"):
                op = "+"
            elif self.eat_sym("-"):
                op = "-"
            else:
                break
            e = ("bin", op, e, self.term())
        return e

    def term(self):
        e = self.unary()
        while True:
            if self.eat_sym("*"):
                op = "*"
            elif self.eat_sym("/"):
                op = "/"
            elif self.eat_sym("%"):
                op = "%"
            else:
                break
            e = ("bin", op, e, self.unary())
        return e

    def unary(self):
        if self.eat_sym("-"):
            return ("neg", self.unary())
        return self.primary()

    def primary(self):
        k, v = self.toks[self.at]
        if k == "num":
            self.at += 1
            return ("lit", v)
        if k == "str":
            self.at += 1
            return ("lit", v)
        if k == "sym":
            if v == "(":
                self.at += 1
                e = self.expr()
                self.expect_sym(")")
                return e
            raise self.fail("expected an expression")
        if k == "ident":
            if self.is_kw("NULL"):
                self.at += 1
                return ("lit", None)
            first = self.name("an expression")
            if self.eat_sym("("):
                fname = first.upper()
                args = []
                if self.eat_sym("*"):
                    args.append(STAR)
                elif not self.is_sym(")"):
                    while True:
                        args.append(self.expr())
                        if not self.eat_sym(","):
                            break
                self.expect_sym(")")
                return ("call", fname, args)
            if self.eat_sym("."):
                return ("col", first, self.name("a column name"))
            return ("col", "", first)
        raise self.fail("expected an expression")


def parse_query(src):
    toks, pos = tokenize(src)
    return Parser(toks, pos).query()


def show(e):
    t = e[0]
    if t == "lit":
        return f"'{e[1]}'" if isinstance(e[1], str) else show_val(e[1])
    if t == "col":
        return e[2] if not e[1] else f"{e[1]}.{e[2]}"
    if t == "star":
        return "*"
    if t == "bin":
        return f"{show(e[2])} {e[1]} {show(e[3])}"
    if t == "not":
        return f"NOT {show(e[1])}"
    if t == "neg":
        return f"-{show(e[1])}"
    if t == "isnull":
        return f"{show(e[1])} IS NOT NULL" if e[2] else f"{show(e[1])} IS NULL"
    if t == "like":
        return f"{show(e[1])} {'NOT LIKE' if e[3] else 'LIKE'} {show(e[2])}"
    if t == "in":
        parts = ", ".join(show(x) for x in e[2])
        return f"{show(e[1])} {'NOT IN' if e[3] else 'IN'} ({parts})"
    return f"{e[1]}({', '.join(show(x) for x in e[2])})"


AGGREGATES = ("COUNT", "SUM", "MIN", "MAX", "AVG")


def has_aggregate(e):
    t = e[0]
    if t == "call":
        return e[1] in AGGREGATES or any(has_aggregate(a) for a in e[2])
    if t == "bin":
        return has_aggregate(e[2]) or has_aggregate(e[3])
    if t in ("not", "neg", "isnull"):
        return has_aggregate(e[1])
    if t == "like":
        return has_aggregate(e[1]) or has_aggregate(e[2])
    if t == "in":
        return has_aggregate(e[1]) or any(has_aggregate(x) for x in e[2])
    return False


# ---- evaluation ----------------------------------------------------------

class Scope:
    def __init__(self, cols, group_cols, grouped):
        self.cols, self.group_cols, self.grouped = cols, group_cols, grouped


def resolve(sc, q, c):
    found = [i for i, (a, n) in enumerate(sc.cols) if (not q or a == q) and n == c]
    shown = c if not q else f"{q}.{c}"
    if not found:
        raise QueryError(f"unknown column '{shown}'")
    if len(found) > 1:
        raise QueryError(f"ambiguous column '{shown}'")
    return found[0]


def like(s, p, i, j):
    if j == len(p):
        return i == len(s)
    if p[j] == "%":
        return any(like(s, p, k, j + 1) for k in range(i, len(s) + 1))
    if i == len(s):
        return False
    if p[j] == "_" or p[j] == s[i]:
        return like(s, p, i + 1, j + 1)
    return False


def trunc_div(x, y):
    q = abs(x) // abs(y)
    return q if (x >= 0) == (y >= 0) else -q


def arith(op, a, b):
    if isinstance(a, int) and isinstance(b, int):
        if op == "+":
            return a + b
        if op == "-":
            return a - b
        if op == "*":
            return a * b
        if b == 0:
            raise QueryError("division by zero")
        if op == "/":
            return trunc_div(a, b)
        return a - b * trunc_div(a, b)
    if a is None or b is None:
        return None
    raise QueryError(f"'{op}' needs numbers, found {show_val(a)} and {show_val(b)}")


def eval_(e, sc, rows, ri):
    t = e[0]
    if t == "lit":
        return e[1]
    if t == "col":
        i = resolve(sc, e[1], e[2])
        if sc.grouped and i not in sc.group_cols:
            raise QueryError(f"column '{show(e)}' must be in GROUP BY or inside an aggregate")
        if ri >= len(rows):
            return None
        return rows[ri][i]
    if t == "star":
        raise QueryError("* is only allowed in COUNT(*) or as the whole select list")
    if t == "bin":
        op = e[1]
        x = eval_(e[2], sc, rows, ri)
        y = eval_(e[3], sc, rows, ri)
        if op in ("AND", "OR"):
            is_and = op == "AND"
            if x is None or y is None:
                if is_and and ((x is not None and not truthy(x)) or (y is not None and not truthy(y))):
                    return 0
                if not is_and and (truthy(x) or truthy(y)):
                    return 1
                return None
            return bool_val(truthy(x) and truthy(y)) if is_and else bool_val(truthy(x) or truthy(y))
        if op in ("+", "-", "*", "/", "%"):
            return arith(op, x, y)
        if x is None or y is None:
            return None
        c = compare(x, y)
        return bool_val({"=": c == 0, "!=": c != 0, "<": c < 0, "<=": c <= 0,
                         ">": c > 0, ">=": c >= 0}[op])
    if t == "not":
        x = eval_(e[1], sc, rows, ri)
        return None if x is None else bool_val(not truthy(x))
    if t == "neg":
        return arith("-", 0, eval_(e[1], sc, rows, ri))
    if t == "isnull":
        return bool_val((eval_(e[1], sc, rows, ri) is None) != e[2])
    if t == "like":
        x = eval_(e[1], sc, rows, ri)
        y = eval_(e[2], sc, rows, ri)
        if isinstance(x, str) and isinstance(y, str):
            return bool_val(like(x, y, 0, 0) != e[3])
        if x is None or y is None:
            return None
        raise QueryError("LIKE needs text")
    if t == "in":
        x = eval_(e[1], sc, rows, ri)
        if x is None:
            return None
        saw_null = False
        for item in e[2]:
            y = eval_(item, sc, rows, ri)
            if y is None:
                saw_null = True
            elif compare(x, y) == 0:
                return bool_val(not e[3])
        return None if saw_null else bool_val(e[3])
    f, args = e[1], e[2]
    if f in AGGREGATES:
        return aggregate(f, args, sc, rows)
    if len(args) != 1:
        raise QueryError(f"{f}() takes one argument")
    x = eval_(args[0], sc, rows, ri)
    if x is None:
        return None
    if isinstance(x, str):
        if f == "UPPER":
            return x.upper()
        if f == "LOWER":
            return x.lower()
        if f == "LENGTH":
            return len(x)
        raise QueryError(f"unknown function {f}()")
    if f == "LENGTH":
        return len(str(x))
    if f in ("UPPER", "LOWER"):
        return str(x)
    raise QueryError(f"unknown function {f}()")


def aggregate(f, args, sc, rows):
    if not sc.grouped:
        raise QueryError(f"aggregate {f}() is not allowed here")
    if len(args) != 1:
        raise QueryError(f"{f}() takes one argument")
    if f == "COUNT" and args[0] == STAR:
        return len(rows)
    inner = Scope(sc.cols, [], False)
    vals = []
    for i in range(len(rows)):
        v = eval_(args[0], inner, rows, i)
        if v is not None:
            vals.append(v)
    if f == "COUNT":
        return len(vals)
    if not vals:
        return None
    if f in ("MIN", "MAX"):
        best = vals[0]
        for v in vals:
            c = compare(v, best)
            if (f == "MIN" and c < 0) or (f == "MAX" and c > 0):
                best = v
        return best
    total = 0
    for v in vals:
        if not isinstance(v, int):
            raise QueryError(f"{f}() needs numbers, found {show_val(v)}")
        total += v
    if f == "SUM":
        return total
    n = len(vals)
    neg = total < 0
    mag = -total if neg else total
    hundredths = (mag * 200 + n) // (2 * n)
    frac = hundredths % 100
    return f"{'-' if neg else ''}{hundredths // 100}.{'0' if frac < 10 else ''}{frac}"


# ---- running a query -----------------------------------------------------

def find_table(db, name):
    for t in db:
        if t.name == name:
            return t
    raise QueryError(f"no table '{name}'")


def wrap(prefix, fn):
    try:
        return fn()
    except QueryError as e:
        raise QueryError(f"{prefix}: {e}")


def compare_keys(a, b, order):
    for i in range(len(order)):
        c = compare(a[i], b[i])
        if c != 0:
            return -c if order[i][1] else c
    return 0


def order_value(e, headers, out, sc, rows):
    if e[0] == "lit" and isinstance(e[1], int):
        n = e[1]
        if n < 1 or n > len(out):
            raise QueryError(f"ORDER BY: there is no column {n}")
        return out[n - 1]
    if e[0] == "col" and not e[1]:
        for i, h in enumerate(headers):
            if h == e[2]:
                return out[i]
    return wrap("ORDER BY", lambda: eval_(e, sc, rows, 0))


def run(db, q):
    base = find_table(db, q["from"])
    base_alias = q["from_alias"] or q["from"]
    cols = [(base_alias, c) for c in base.cols]
    rows = [list(r) for r in base.rows]
    for table, alias, on, left in q["joins"]:
        t = find_table(db, table)
        alias = alias or table
        cols = cols + [(alias, c) for c in t.cols]
        sc = Scope(cols, [], False)
        joined = []
        for r in rows:
            matched = False
            for s in t.rows:
                both = r + s
                ok = wrap("ON", lambda: eval_(on, sc, [both], 0))
                if truthy(ok):
                    joined.append(both)
                    matched = True
            if not matched and left:
                joined.append(r + [None] * len(t.cols))
        rows = joined
    plain = Scope(cols, [], False)
    if q["filter"] is not None:
        kept = []
        for i in range(len(rows)):
            if truthy(wrap("WHERE", lambda: eval_(q["filter"], plain, rows, i))):
                kept.append(rows[i])
        rows = kept

    grouped = (bool(q["group"]) or any(has_aggregate(e) for e, _ in q["items"])
               or q["having"] is not None or any(has_aggregate(e) for e, _ in q["order"]))
    units = []
    group_cols = []
    if grouped:
        for g in q["group"]:
            if g[0] != "col":
                raise QueryError(f"GROUP BY: '{show(g)}' is not a column")
            group_cols.append(wrap("GROUP BY", lambda: resolve(plain, g[1], g[2])))
        if not q["group"]:
            units.append(rows)
        else:
            keys = []
            for r in rows:
                key = [r[i] for i in group_cols]
                if key in keys:
                    units[keys.index(key)].append(r)
                else:
                    keys.append(key)
                    units.append([r])
    else:
        units = [[r] for r in rows]
    sc = Scope(cols, group_cols, grouped)

    headers = []
    for e, alias in q["items"]:
        if e == STAR:
            if grouped:
                raise QueryError("SELECT: * cannot be used in a grouped query")
            headers += [c for _, c in cols]
        else:
            headers.append(alias or show(e))

    kept = []
    for u in units:
        if q["having"] is not None:
            if not truthy(wrap("HAVING", lambda: eval_(q["having"], sc, u, 0))):
                continue
        out = []
        for e, _ in q["items"]:
            if e == STAR:
                out += u[0]
            else:
                out.append(wrap("SELECT", lambda: eval_(e, sc, u, 0)))
        keys = [order_value(e, headers, out, sc, u) for e, _ in q["order"]]
        if q["distinct"] and any(x[1] == out for x in kept):
            continue
        kept.append((u, out, keys))
    # A stable insertion sort.
    ordered = []
    for k in kept:
        j = len(ordered)
        while j > 0 and compare_keys(ordered[j - 1][2], k[2], q["order"]) > 0:
            j -= 1
        ordered.insert(j, k)
    result = []
    for i, (_, out, _) in enumerate(ordered):
        if i < q["offset"]:
            continue
        if q["limit"] is not None and len(result) >= q["limit"]:
            break
        result.append(out)
    return headers, result


# ---- output --------------------------------------------------------------

def print_table(headers, rows):
    widths = [len(h) for h in headers]
    for r in rows:
        for i, v in enumerate(r):
            widths[i] = max(widths[i], len(show_val(v)))
    line = " | ".join(h.ljust(widths[i]) for i, h in enumerate(headers))
    rule = "-+-".join("-" * w for w in widths)
    print(f"  {line.rstrip()}")
    print(f"  {rule}")
    for r in rows:
        cells = []
        for i, v in enumerate(r):
            cell = show_val(v)
            cells.append(cell.rjust(widths[i]) if isinstance(v, int) else cell.ljust(widths[i]))
        print(f"  {' | '.join(cells).rstrip()}")
    print(f"  ({len(rows)} row{'' if len(rows) == 1 else 's'})")


CUSTOMERS = """id,name,city,tier
1,Ada Lovelace,London,gold
2,"Grace Hopper",New York,silver
3,Alan Turing,London,
4,"Hopper, Grace Jr.",Boston,gold
5,Edsger Dijkstra,Austin,bronze
"""

ORDERS = """order_id,customer_id,item,qty,price_cents
100,1,keyboard,2,4999
101,2,mouse,1,1999
102,1,monitor,1,18999
103,3,"cable ""usb-c""\",5,799
104,9,adapter,3,
105,4,mouse,2,1999
106,2,keyboard,1,4999
107,5,dock,1,12999
108,3,mouse,,1999
"""

UNCLOSED = 'x,y\n1,"open\n'

BROKEN = """a,b,c
1,2,3
4,5
"""

QUERIES = [
    "SELECT * FROM customers",
    "SELECT name, city FROM customers WHERE city = 'London' OR tier IS NULL ORDER BY name",
    "SELECT item, qty * price_cents AS total FROM orders WHERE price_cents IS NOT NULL AND qty >= 2 ORDER BY total DESC",
    "SELECT c.name, o.item, o.qty FROM orders o JOIN customers c ON o.customer_id = c.id ORDER BY c.name, o.order_id LIMIT 5",
    "SELECT c.name, COUNT(o.order_id) AS orders, SUM(o.qty * o.price_cents) AS spent FROM customers c LEFT JOIN orders o ON o.customer_id = c.id GROUP BY c.name ORDER BY spent DESC, 1",
    "SELECT city, COUNT(*) AS n, AVG(id) FROM customers GROUP BY city HAVING COUNT(*) > 1",
    "SELECT DISTINCT item FROM orders ORDER BY item",
    "SELECT UPPER(name), LENGTH(name) AS len FROM customers WHERE name LIKE '%Hopper%'",
    "SELECT order_id, item FROM orders WHERE item IN ('mouse', 'dock') AND customer_id NOT IN (2)",
    "SELECT order_id, qty FROM orders WHERE qty > 1 OR qty IS NULL ORDER BY qty, order_id",
    "select tier, min(name), max(id), count(tier) from customers group by tier order by tier",
    "SELECT item, price_cents / qty AS each, price_cents % 1000 FROM orders WHERE order_id <= 101 OR item LIKE 'c_ble%'",
    "SELECT COUNT(*), SUM(qty), AVG(price_cents) FROM orders WHERE customer_id = 1",
    "SELECT COUNT(*), SUM(qty), MAX(item) FROM orders WHERE customer_id = 42",
    "SELECT name FROM customers ORDER BY id DESC LIMIT 2 OFFSET 1",
    "SELECT o.order_id FROM orders o LEFT JOIN customers c ON c.id = o.customer_id WHERE c.id IS NULL",
    "SELECT name, NOT (tier = 'gold') AS not_gold, -id FROM customers WHERE tier != 'silver' OR tier IS NULL",
    "SELECT nam FROM customers",
    "SELECT name FROM customers a JOIN customers b ON a.id = b.id",
    "SELECT city, name FROM customers GROUP BY city",
    "SELECT * FROM customers WHERE COUNT(*) > 1",
    "SELECT name FROM customers WHERE",
    "SELECT name FROM nowhere",
    "SELECT 'unterminated FROM customers",
    "SELECT qty / 0 FROM orders LIMIT 1",
    "SELECT name FROM customers ORDER BY 3",
    "SELECT name, FROM customers",
    "SELECT name FROM customers WHERE city = 'London' extra",
    "SELECT name + 1 FROM customers",
    "SELECT * FROM customers WHERE id # 2",
]


def main():
    files = [("customers", CUSTOMERS), ("orders", ORDERS), ("broken", BROKEN), ("unclosed", UNCLOSED)]
    db = []
    print("== load")
    for name, text in files:
        path = f"{name}.csv"
        with open(path, "w") as f:
            f.write(text)
        with open(path) as f:
            read = f.read()
        try:
            t = load_csv(name, read)
        except QueryError as e:
            print(f"  error: {e}")
            continue
        desc = ", ".join(f"{t.cols[i]} {t.types[i]}" for i in range(len(t.cols)))
        print(f"  {t.name}: {desc} ({len(t.rows)} rows)")
        db.append(t)
    for src in QUERIES:
        print(f"== {src}")
        try:
            q = parse_query(src)
            headers, rows = run(db, q)
        except QueryError as e:
            print(f"  error: {e}")
            continue
        print_table(headers, rows)


main()
