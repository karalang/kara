"""Mirror of inventory.kara: an inventory service over two stores."""


def money(c):
    sign = "-" if c < 0 else ""
    c = abs(c)
    return f"{sign}${c // 100}.{c % 100:02d}"


def percent_off(c, pct):
    return (c * pct + 50) // 100


class Item:
    def __init__(self, sku, name, on_hand, reserved, price):
        self.sku, self.name, self.on_hand, self.reserved, self.price = sku, name, on_hand, reserved, price

    def copy(self):
        return Item(self.sku, self.name, self.on_hand, self.reserved, self.price)

    def available(self):
        return self.on_hand - self.reserved


class StoreError(Exception):
    pass


def full(n):
    return StoreError(f"store full ({n} items)")


class MemStore:
    def __init__(self, limit):
        self.items, self.limit = {}, limit

    def get(self, sku):
        i = self.items.get(sku)
        return None if i is None else i.copy()

    def put(self, item):
        if item.sku not in self.items and len(self.items) >= self.limit:
            raise full(self.limit)
        self.items[item.sku] = item

    def remove(self, sku):
        return self.items.pop(sku, None) is not None

    def skus(self):
        return sorted(self.items)

    def label(self):
        return "memory"


def encode_item(i):
    return f"put|{i.sku}|{i.name}|{i.on_hand}|{i.reserved}|{i.price}"


def decode_line(line):
    f = line.split("|")
    if len(f) == 2 and f[0] == "del":
        return f[1], None
    if len(f) != 6 or f[0] != "put":
        return None
    try:
        return f[1], Item(f[1], f[2], int(f[3]), int(f[4]), int(f[5]))
    except ValueError:
        return None


class FileStore:
    def __init__(self, path, limit):
        self.path, self.cache, self.log_lines, self.compactions, self.limit = path, {}, 0, 0, limit
        try:
            with open(path) as fh:
                text = fh.read()
        except FileNotFoundError:
            text = ""
        lines = text.splitlines()
        torn = False
        for k, line in enumerate(lines):
            d = decode_line(line)
            if d is None:
                if k + 1 == len(lines):
                    torn = True
                else:
                    raise StoreError(f"io: corrupt log line {k + 1}")
            elif d[1] is None:
                self.cache.pop(d[0], None)
            else:
                self.cache[d[0]] = d[1]
            self.log_lines += 1
        if torn or (not text.endswith("\n") and len(text) > 0):
            print(f"  file store: dropped a torn last line, {len(self.cache)} items recovered")
            self.compact()

    def append(self, line):
        try:
            with open(self.path) as fh:
                text = fh.read()
        except FileNotFoundError:
            text = ""
        with open(self.path, "w") as fh:
            fh.write(text + line + "\n")
        self.log_lines += 1
        if self.log_lines > 2 * len(self.cache) + 8:
            self.compact()

    def compact(self):
        with open(self.path, "w") as fh:
            fh.write("".join(encode_item(self.cache[k]) + "\n" for k in sorted(self.cache)))
        self.log_lines = len(self.cache)
        self.compactions += 1

    def get(self, sku):
        i = self.cache.get(sku)
        return None if i is None else i.copy()

    def put(self, item):
        if item.sku not in self.cache and len(self.cache) >= self.limit:
            raise full(self.limit)
        line = encode_item(item)
        self.cache[item.sku] = item
        self.append(line)

    def remove(self, sku):
        if self.cache.pop(sku, None) is None:
            return False
        self.append(f"del|{sku}")
        return True

    def skus(self):
        return sorted(self.cache)

    def label(self):
        return "file"


def discount_pct(sub):
    return 10 if sub >= 50000 else 5 if sub >= 20000 else 0


class Service:
    def __init__(self, store):
        self.store, self.orders, self.seen, self.audit = store, {}, {}, []

    def stock(self, sku, name, qty, price):
        i = self.store.get(sku)
        if i is not None:
            i.on_hand += qty
            i.price = price
        else:
            i = Item(sku, name, qty, 0, price)
        try:
            self.store.put(i)
        except StoreError as e:
            return f"error {e}"
        self.audit.append(f"stock {sku} +{qty}")
        return f"ok {sku} on_hand={i.on_hand}"

    def quote(self, lines):
        sub = 0
        for sku, qty in lines:
            i = self.store.get(sku)
            if i is None:
                raise StoreError(f"unknown sku {sku}")
            sub += i.price * qty
        return sub, sub - percent_off(sub, discount_pct(sub))

    def reserve(self, oid, lines):
        if oid in self.orders:
            return f"error order {oid} exists"
        for sku, qty in lines:
            i = self.store.get(sku)
            if i is None:
                return f"error unknown sku {sku}"
            if qty <= 0:
                return f"error bad quantity {qty} for {sku}"
            if i.available() < qty:
                return f"error short {sku}: want {qty}, have {i.available()}"
        sub, total = self.quote(lines)
        for sku, qty in lines:
            i = self.store.get(sku)
            i.reserved += qty
            self.store.put(i)
        self.audit.append(f"reserve {oid}")
        self.orders[oid] = [lines, "reserved"]
        return f"ok {oid} subtotal={money(sub)} total={money(total)}"

    def finish(self, oid, commit):
        o = self.orders.get(oid)
        if o is None:
            return f"error no order {oid}"
        if o[1] != "reserved":
            return f"error order {oid} is {o[1]}"
        for sku, qty in o[0]:
            i = self.store.get(sku)
            i.reserved -= qty
            if commit:
                i.on_hand -= qty
            self.store.put(i)
        state = "shipped" if commit else "cancelled"
        o[1] = state
        self.audit.append(f"{state} {oid}")
        return f"ok {oid} {state}"

    def run(self, key, cmd):
        if key in self.seen:
            return f"{self.seen[key]} (replayed)"
        w = cmd.split(" ")
        if w[0] == "stock" and len(w) == 5:
            reply = self.stock(w[1], w[2], int(w[3]), int(w[4]))
        elif w[0] in ("reserve", "quote") and len(w) >= 3:
            lines = []
            for tok in w[2:]:
                parts = tok.split("x")
                if len(parts) == 2:
                    lines.append((parts[0], int(parts[1])))
            if w[0] == "quote":
                try:
                    s, t = self.quote(lines)
                    reply = f"ok quote subtotal={money(s)} total={money(t)}"
                except StoreError as e:
                    reply = f"error {e}"
            else:
                reply = self.reserve(w[1], lines)
        elif w[0] == "ship" and len(w) == 2:
            reply = self.finish(w[1], True)
        elif w[0] == "cancel" and len(w) == 2:
            reply = self.finish(w[1], False)
        elif w[0] == "drop" and len(w) == 2:
            i = self.store.get(w[1])
            ok = i is not None and i.reserved == 0 and self.store.remove(w[1])
            reply = f"ok dropped {w[1]}" if ok else f"error cannot drop {w[1]}"
        else:
            reply = f"error bad command [{cmd}]"
        self.seen[key] = reply
        return reply

    def report(self):
        out, value, avail = [], 0, []
        for sku in self.store.skus():
            i = self.store.get(sku)
            value += i.price * i.on_hand
            out.append(f"{i.sku} {i.name} on_hand={i.on_hand} reserved={i.reserved} price={money(i.price)}")
            avail.append((i.available(), i.sku))
        out.append(f"stock value {money(value)}")
        top = sorted(avail, reverse=True)[:2]
        out.append("most available: " + ", ".join(f"{s}={n}" for n, s in top))
        states = {}
        for o in self.orders.values():
            states[o[1]] = states.get(o[1], 0) + 1
        out.append("orders: " + " ".join(f"{s}={states[s]}" for s in sorted(states)))
        return out


SCRIPT = """r1 stock KB-1 keyboard 10 4999
r2 stock MS-2 mouse 25 1999
r3 stock MN-3 monitor 4 18900
r4 stock CB-4 cable 100 499
r5 quote q KB-1x2 MS-2x2
r6 reserve o1 KB-1x2 MS-2x2 CB-4x5
r7 reserve o2 MN-3x3 KB-1x1
r8 reserve o3 MN-3x2 CB-4x1
r9 reserve o4 KB-1x1 XX-9x1
r6 reserve o1 KB-1x2 MS-2x2 CB-4x5
r10 ship o1
r11 ship o1
r12 cancel o2
r13 reserve o3 MN-3x2 CB-4x1
r14 stock MN-3 monitor 2 17900
r15 reserve o5 MN-3x0
r16 drop MS-2
r17 drop CB-4
r18 stock HB-5 hub 3 2599
r19 stock DK-6 dock 1 15900
r20 frobnicate
r21 ship o3
r22 quote q MN-3x3 KB-1x1
"""


def drive(svc, script):
    out = []
    for line in script.splitlines():
        sp = line.find(" ")
        if sp >= 0:
            out.append(f"{line[:sp]}: {svc.run(line[:sp], line[sp + 1:])}")
    return out


def read(path):
    with open(path) as fh:
        return fh.read()


def write(path, text):
    with open(path, "w") as fh:
        fh.write(text)


def main():
    write("orders.txt", SCRIPT)
    script = read("orders.txt")
    mem = Service(MemStore(4))
    print(f"== {mem.store.label()} store")
    a = drive(mem, script)
    for l in a:
        print(f"  {l}")
    for l in mem.report():
        print(f"  | {l}")

    file = Service(FileStore("inventory.log", 4))
    print(f"== {file.store.label()} store")
    b = drive(file, script)
    print(f"  transcript identical to memory store: {'true' if a == b else 'false'}")
    print(f"  log lines={file.store.log_lines} compactions={file.store.compactions}")
    print(f"  audit: {len(file.audit)} events, last: {file.audit[-1]}")

    print("== reopen from disk")
    again = Service(FileStore("inventory.log", 4))
    for l in again.report():
        print(f"  | {l}")

    print("== torn write")
    write("inventory.log", read("inventory.log") + "put|KB-1|keyboard|9")
    svc = Service(FileStore("inventory.log", 4))
    print(f"  | {svc.report()[0]}")
    print(f"  log after recovery: {len(read('inventory.log').splitlines())} lines")
    print("== corrupt middle line")
    text = read("inventory.log").replace("put|KB-1", "pot|KB-1", 1) + "del|HB-5\n"
    write("inventory.log", text)
    try:
        FileStore("inventory.log", 4)
        print("  opened a corrupt log")
    except StoreError as e:
        print(f"  open refused: {e}")


main()
