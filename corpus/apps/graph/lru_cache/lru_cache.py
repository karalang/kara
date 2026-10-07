# Python mirror of source.orig.kara: LRU cache = doubly linked list + dict index.


class Node:
    def __init__(self, key, value):
        self.key = key
        self.value = value
        self.prev = None
        self.next = None


CAPACITY, DELETED, SHRUNK = "capacity", "deleted", "shrunk"


class ScriptError(Exception):
    pass


class Listener:
    def __init__(self, name, on_evict):
        self.name = name
        self.on_evict = on_evict


class Stats:
    def __init__(self):
        self.hits = 0
        self.misses = 0
        self.evictions = 0
        self.deletes = 0
        self.inserts = 0
        self.updates = 0


class LruCache:
    def __init__(self, capacity):
        self.capacity = capacity
        self.index = {}
        self.head = None
        self.tail = None
        self.stats = Stats()
        self.listeners = []

    def on_evict(self, listener):
        self.listeners.append(listener)

    def __len__(self):
        return len(self.index)

    def unlink(self, node):
        if node.prev is not None:
            node.prev.next = node.next
        else:
            self.head = node.next
        if node.next is not None:
            node.next.prev = node.prev
        else:
            self.tail = node.prev
        node.prev = None
        node.next = None

    def push_front(self, node):
        node.prev = None
        node.next = self.head
        if self.head is not None:
            self.head.prev = node
        else:
            self.tail = node
        self.head = node

    def touch(self, node):
        self.unlink(node)
        self.push_front(node)

    def notify(self, key, value, reason):
        for listener in self.listeners:
            msg = listener.on_evict(key, value, reason)
            if msg is not None:
                print(f"    [{listener.name}] {msg}")

    def evict_lru(self, reason):
        victim = self.tail
        if victim is None:
            return
        self.unlink(victim)
        del self.index[victim.key]
        self.stats.evictions += 1
        print(f"  evict {victim.key}={victim.value} ({reason})")
        self.notify(victim.key, victim.value, reason)

    def get(self, key):
        node = self.index.get(key)
        if node is not None:
            self.stats.hits += 1
            self.touch(node)
            return node.value
        self.stats.misses += 1
        return None

    def peek(self, key):
        node = self.index.get(key)
        return node.value if node is not None else None

    def put(self, key, value):
        node = self.index.get(key)
        if node is not None:
            node.value = value
            self.stats.updates += 1
            self.touch(node)
            return
        if len(self) >= self.capacity:
            self.evict_lru(CAPACITY)
        node = Node(key, value)
        self.index[key] = node
        self.push_front(node)
        self.stats.inserts += 1

    def delete(self, key):
        node = self.index.pop(key, None)
        if node is None:
            return False
        self.unlink(node)
        self.stats.deletes += 1
        self.notify(node.key, node.value, DELETED)
        return True

    def resize(self, capacity):
        self.capacity = capacity
        while len(self) > self.capacity:
            self.evict_lru(SHRUNK)

    def order(self):
        out = []
        cur = self.head
        while cur is not None:
            out.append(f"{cur.key}={cur.value}")
            cur = cur.next
        return out


def parse_number(line, text):
    try:
        return int(text)
    except ValueError:
        raise ScriptError(f"line {line}: '{text}' is not a number")


def arg(words, i, line):
    if i < len(words):
        return words[i]
    raise ScriptError(f"line {line}: '{words[0]}' is missing an argument")


def parse_line(line, text):
    words = text.split(" ")
    cmd = words[0]
    if cmd == "get":
        return ("get", arg(words, 1, line))
    if cmd == "peek":
        return ("peek", arg(words, 1, line))
    if cmd == "put":
        key = arg(words, 1, line)
        value = parse_number(line, arg(words, 2, line))
        return ("put", key, value)
    if cmd == "del":
        return ("del", arg(words, 1, line))
    if cmd == "resize":
        return ("resize", parse_number(line, arg(words, 1, line)))
    if cmd == "dump":
        return ("dump",)
    raise ScriptError(f"line {line}: unknown command '{cmd}'")


def parse_script(source):
    commands, errors = [], []
    for line_no, raw in enumerate(source.split("\n"), start=1):
        text = raw.strip()
        if text == "" or text.startswith("#"):
            continue
        try:
            commands.append(parse_line(line_no, text))
        except ScriptError as e:
            errors.append(str(e))
    return commands, errors


def show(v):
    return "miss" if v is None else f"{v}"


def run_script(name, capacity, source):
    print(f"== {name} (capacity {capacity}) ==")
    commands, errors = parse_script(source)
    for e in errors:
        print(f"  skip {e}")

    cache = LruCache(capacity)
    cache.on_evict(Listener("audit", lambda key, value, reason: f"{key} left with {value} ({reason})"))
    cache.on_evict(Listener("big", lambda key, value, reason:
                            f"large value {value} under {key} dropped" if value >= 100 else None))

    for cmd in commands:
        op = cmd[0]
        if op == "get":
            print(f"  get {cmd[1]} -> {show(cache.get(cmd[1]))}")
        elif op == "peek":
            print(f"  peek {cmd[1]} -> {show(cache.peek(cmd[1]))}")
        elif op == "put":
            print(f"  put {cmd[1]}={cmd[2]}")
            cache.put(cmd[1], cmd[2])
        elif op == "del":
            removed = cache.delete(cmd[1])
            print(f"  del {cmd[1]} -> {'true' if removed else 'false'}")
        elif op == "resize":
            print(f"  resize {cmd[1]}")
            cache.resize(cmd[1])
        elif op == "dump":
            print(f"  order [{', '.join(cache.order())}]")

    s = cache.stats
    total = s.hits + s.misses
    rate = 0 if total == 0 else s.hits * 100 // total
    print(f"  hits {s.hits}, misses {s.misses}, hit rate {rate}%")
    print(f"  inserts {s.inserts}, updates {s.updates}, deletes {s.deletes}, evictions {s.evictions}")
    print(f"  final order [{', '.join(cache.order())}] size {len(cache)}/{cache.capacity}")


def main():
    session_script = "# web session cache\nput alice 10\nput bob 20\nput carol 30\nget alice\nput dave 40\nget bob\nget carol\npeek alice\nput alice 110\nput erin 50\ndump\ndel carol\ndel zed\nget erin\nput ivy 60\nfetch alice\nput frank\nresize 2\nget alice\nput gina 120\nput hank 7\n"
    query_script = "# query result cache\nput q1 5\nput q2 150\nget q1\nput q3 9\nget q2\nput q1 x\nput q4 1\nget q3\nresize 1\nput q5 300\nget q1\ndump\n"

    with open("session.script", "w") as f:
        f.write(session_script)
    with open("query.script", "w") as f:
        f.write(query_script)

    with open("session.script") as f:
        session = f.read()
    with open("query.script") as f:
        query = f.read()

    run_script("sessions", 3, session)
    run_script("queries", 2, query)


main()
