# The mirror of mini_memcached: the same server, fed the same session
# bytes. expected.out is its output.
import shutil
import mk

START = 1760000000
PROCESS_STARTED = START - 2
REALTIME_MAXDELTA = 60 * 60 * 24 * 30
LIMIT = 2048
OVERHEAD = 48
KEY_MAX = 250
MASK = (1 << 64) - 1


def show(b):
    s = b.decode("latin-1")
    if len(s) > 60:
        s = s[:20] + "...(%d bytes)" % len(s)
    return s.replace("\r", "\\r").replace("\n", "\\n")


class Item:
    def __init__(self, key, flags, exptime, data, cas, time):
        self.key = key
        self.flags = flags
        self.exptime = exptime
        self.data = data
        self.cas = cas
        self.time = time

    def size(self):
        return OVERHEAD + len(self.key) + len(self.data)


class Cache:
    def __init__(self):
        self.now = 2
        self.items = {}
        self.lru = []  # most recent last
        self.next_cas = 1
        self.oldest_live = 0
        self.bytes = 0
        self.stats = {k: 0 for k in [
            "cmd_get", "cmd_set", "cmd_touch", "get_hits", "get_misses", "get_expired",
            "delete_misses", "delete_hits", "incr_misses", "incr_hits", "decr_misses",
            "decr_hits", "cas_misses", "cas_hits", "cas_badval", "touch_hits",
            "touch_misses", "cmd_flush", "evictions", "reclaimed", "total_items"]}
        self.connections = 0

    def realtime(self, exptime):
        if exptime < 0:
            exptime = REALTIME_MAXDELTA + 1
        if exptime == 0:
            return 0
        if exptime > REALTIME_MAXDELTA:
            if exptime <= PROCESS_STARTED:
                return 1
            return exptime - PROCESS_STARTED
        return exptime + self.now

    def dead(self, it):
        if it.exptime != 0 and it.exptime <= self.now:
            return "expired"
        if self.oldest_live != 0 and self.oldest_live <= self.now and it.time < self.oldest_live:
            return "flushed"
        return None

    def unlink(self, key):
        it = self.items.pop(key)
        self.lru.remove(key)
        self.bytes -= it.size()

    def bump(self, key):
        self.lru.remove(key)
        self.lru.append(key)

    def find(self, key):
        it = self.items.get(key)
        if it is None:
            return None
        why = self.dead(it)
        if why:
            if why == "expired":
                self.stats["get_expired"] += 1
            self.unlink(key)
            return None
        return it

    def link(self, it):
        if it.key in self.items:
            self.unlink(it.key)
        while self.bytes + it.size() > LIMIT and self.lru:
            victim = self.items[self.lru[0]]
            if self.dead(victim):
                self.stats["reclaimed"] += 1
            else:
                self.stats["evictions"] += 1
            self.unlink(victim.key)
        it.cas = self.next_cas
        self.next_cas += 1
        it.time = self.now
        self.items[it.key] = it
        self.lru.append(it.key)
        self.bytes += it.size()
        self.stats["total_items"] += 1


def parse_num(s, lo, hi):
    if s.startswith("-"):
        body = s[1:]
    else:
        body = s
    if body == "" or not body.isdigit() or not body.isascii():
        return None
    v = int(s)
    if v < lo or v > hi:
        return None
    return v


def serve(cache, data, out):
    """Handles one connection's bytes; returns the reply bytes."""
    reply = []
    pos = 0

    def say(line):
        reply.append(line + "\r\n")
        out.append("< " + show(line.encode("latin-1")))

    while pos < len(data):
        nl = data.find(b"\n", pos)
        if nl < 0:
            break
        line = data[pos:nl]
        pos = nl + 1
        if line.endswith(b"\r"):
            line = line[:-1]
        text = line.decode("latin-1")
        out.append("> " + show(line))
        toks = text.split()
        if not toks:
            say("ERROR")
            continue
        cmd = toks[0]
        noreply = toks[-1] == "noreply"
        args = toks[1:-1] if noreply else toks[1:]

        def answer(line):
            if noreply:
                out.append("  (noreply: %s)" % line)
            else:
                say(line)

        if cmd in ("set", "add", "replace", "append", "prepend", "cas"):
            need = 5 if cmd == "cas" else 4
            if len(args) != need:
                say("ERROR")
                continue
            key = args[0]
            flags = parse_num(args[1], 0, (1 << 32) - 1)
            exptime = parse_num(args[2], -(1 << 31), (1 << 31) - 1)
            vlen = parse_num(args[3], -(1 << 31), (1 << 31) - 1)
            casv = parse_num(args[4], 0, MASK) if cmd == "cas" else 0
            if len(key) > KEY_MAX or flags is None or exptime is None or vlen is None or casv is None or vlen < 0:
                say("CLIENT_ERROR bad command line format")
                continue
            block = data[pos:pos + vlen + 2]
            pos += vlen + 2
            out.append(">   " + show(block[:vlen]))
            if OVERHEAD + len(key) + vlen > LIMIT:
                say("SERVER_ERROR object too large for cache")
                continue
            if len(block) != vlen + 2 or block[vlen:] != b"\r\n":
                say("CLIENT_ERROR bad data chunk")
                continue
            value = block[:vlen]
            cache.stats["cmd_set"] += 1
            old = cache.find(key)
            new = Item(key, flags, cache.realtime(exptime), value, 0, cache.now)
            if cmd == "set":
                cache.link(new)
                answer("STORED")
            elif cmd == "add":
                if old is not None:
                    cache.bump(key)
                    answer("NOT_STORED")
                else:
                    cache.link(new)
                    answer("STORED")
            elif cmd == "replace":
                if old is None:
                    answer("NOT_STORED")
                else:
                    cache.link(new)
                    answer("STORED")
            elif cmd in ("append", "prepend"):
                if old is None:
                    answer("NOT_STORED")
                else:
                    joined = old.data + value if cmd == "append" else value + old.data
                    cache.link(Item(key, old.flags, old.exptime, joined, 0, cache.now))
                    answer("STORED")
            else:
                if old is None:
                    cache.stats["cas_misses"] += 1
                    answer("NOT_FOUND")
                elif old.cas != casv:
                    cache.stats["cas_badval"] += 1
                    answer("EXISTS")
                else:
                    cache.stats["cas_hits"] += 1
                    cache.link(new)
                    answer("STORED")
        elif cmd in ("get", "gets", "gat", "gats"):
            keys = toks[1:]
            touch = cmd in ("gat", "gats")
            exptime = None
            if touch:
                if not keys:
                    say("ERROR")
                    continue
                exptime = parse_num(keys[0], -(1 << 31), (1 << 31) - 1)
                keys = keys[1:]
                if exptime is None:
                    say("CLIENT_ERROR invalid exptime argument")
                    continue
            if not keys:
                say("ERROR")
                continue
            if any(len(k) > KEY_MAX for k in keys):
                say("CLIENT_ERROR bad command line format")
                continue
            for k in keys:
                if touch:
                    cache.stats["cmd_touch"] += 1
                else:
                    cache.stats["cmd_get"] += 1
                it = cache.find(k)
                if it is None:
                    cache.stats["touch_misses" if touch else "get_misses"] += 1
                    continue
                cache.stats["touch_hits" if touch else "get_hits"] += 1
                if touch:
                    it.exptime = cache.realtime(exptime)
                cache.bump(k)
                head = "VALUE %s %d %d" % (k, it.flags, len(it.data))
                if cmd in ("gets", "gats"):
                    head += " %d" % it.cas
                say(head)
                reply.append(it.data.decode("latin-1") + "\r\n")
                out.append("<   " + show(it.data))
            say("END")
        elif cmd == "delete":
            if len(args) == 2 and args[1] == "0":
                args = args[:1]
            if len(args) != 1:
                say("CLIENT_ERROR bad command line format.  Usage: delete <key> [noreply]")
                continue
            if cache.find(args[0]) is not None:
                cache.unlink(args[0])
                cache.stats["delete_hits"] += 1
                answer("DELETED")
            else:
                cache.stats["delete_misses"] += 1
                answer("NOT_FOUND")
        elif cmd in ("incr", "decr"):
            if len(args) != 2:
                say("ERROR")
                continue
            delta = parse_num(args[1], 0, MASK)
            if delta is None:
                say("CLIENT_ERROR invalid numeric delta argument")
                continue
            it = cache.find(args[0])
            if it is None:
                cache.stats[cmd + "_misses"] += 1
                answer("NOT_FOUND")
                continue
            cur = parse_num(it.data.decode("latin-1"), 0, MASK) if len(it.data) <= 20 else None
            if cur is None:
                say("CLIENT_ERROR cannot increment or decrement non-numeric value")
                continue
            cache.stats[cmd + "_hits"] += 1
            if cmd == "incr":
                v = (cur + delta) & MASK
            else:
                v = max(cur - delta, 0)
            cache.link(Item(it.key, it.flags, it.exptime, str(v).encode(), 0, cache.now))
            answer(str(v))
        elif cmd == "touch":
            if len(args) != 2:
                say("ERROR")
                continue
            exptime = parse_num(args[1], -(1 << 31), (1 << 31) - 1)
            if exptime is None:
                say("CLIENT_ERROR invalid exptime argument")
                continue
            cache.stats["cmd_touch"] += 1
            it = cache.find(args[0])
            if it is None:
                cache.stats["touch_misses"] += 1
                answer("NOT_FOUND")
            else:
                cache.stats["touch_hits"] += 1
                it.exptime = cache.realtime(exptime)
                cache.bump(args[0])
                answer("TOUCHED")
        elif cmd == "flush_all":
            delay = parse_num(args[0], 0, (1 << 31) - 1) if args else 0
            if delay is None:
                say("CLIENT_ERROR bad command line format")
                continue
            cache.stats["cmd_flush"] += 1
            if delay == 0:
                for k in list(cache.lru):
                    cache.unlink(k)
                cache.oldest_live = 0
            else:
                cache.oldest_live = cache.realtime(delay)
            answer("OK")
        elif cmd == "version":
            say("VERSION 1.6.21")
        elif cmd == "verbosity":
            answer("OK")
        elif cmd == "stats":
            st = cache.stats
            say("STAT time %d" % (PROCESS_STARTED + cache.now))
            say("STAT total_connections %d" % cache.connections)
            for k in ["cmd_get", "cmd_set", "cmd_flush", "cmd_touch", "get_hits", "get_misses",
                      "get_expired", "delete_misses", "delete_hits", "incr_misses", "incr_hits",
                      "decr_misses", "decr_hits", "cas_misses", "cas_hits", "cas_badval",
                      "touch_hits", "touch_misses"]:
                say("STAT %s %d" % (k, st[k]))
            say("STAT limit_maxbytes %d" % LIMIT)
            say("STAT bytes %d" % cache.bytes)
            say("STAT curr_items %d" % len(cache.items))
            say("STAT total_items %d" % st["total_items"])
            say("STAT evictions %d" % st["evictions"])
            say("STAT reclaimed %d" % st["reclaimed"])
            say("END")
        elif cmd == "quit":
            out.append("  (connection closed by quit)")
            break
        else:
            say("ERROR")
    return "".join(reply).encode("latin-1"), pos


def main():
    shutil.rmtree("data", ignore_errors=True)
    mk.write_all()
    text = open("data/sessions.txt").read()
    sessions = []
    for line in text.split("\n"):
        if line.startswith("=== "):
            sessions.append([line[4:], 0, []])
        elif line.startswith("@advance "):
            sessions[-1][1] = int(line[9:])
        elif line or sessions[-1][2]:
            sessions[-1][2].append(line)
    cache = Cache()
    print("mini_memcached: listening on 127.0.0.1:47747, %d bytes of memory" % LIMIT)
    for name, advance, lines in sessions:
        while lines and lines[-1] == "":
            lines.pop()
        cache.now += advance
        cache.connections += 1
        payload = "".join(l + "\r\n" for l in lines).encode("latin-1")
        print("=== %s (t+%d)" % (name, cache.now - 2))
        out = []
        reply, used = serve(cache, payload, out)
        for l in out:
            print(l)
        print("--- client sent %d bytes, server read %d, client got %d" % (len(payload), used, len(reply)))
    print("mini_memcached: %d items, %d bytes" % (len(cache.items), cache.bytes))


main()
