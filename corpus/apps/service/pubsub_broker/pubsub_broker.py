"""Python mirror of source.orig.kara: topic pub/sub broker with wildcard
subscriptions, bounded drop-oldest mailboxes and retained messages."""
from collections import deque


class Message:
    def __init__(self, id, topic, payload, retained):
        self.id = id
        self.topic = topic
        self.payload = payload
        self.retained = retained

    def copy_as(self, retained):
        return Message(self.id, self.topic, self.payload, retained)

    def render(self):
        flag = " [retained]" if self.retained else ""
        return f"#{self.id} {self.topic} = {self.payload}{flag}"


class BrokerError(Exception):
    pass


def validate_topic(topic):
    segs = topic.split(".")
    for seg in segs:
        if seg == "" or "*" in seg or "#" in seg:
            raise BrokerError(f"invalid topic '{topic}'")
    return segs


def validate_pattern(pattern):
    segs = pattern.split(".")
    last = len(segs) - 1
    for i, seg in enumerate(segs):
        wildcard = seg == "*" or seg == "#"
        if seg == "":
            raise BrokerError(f"invalid pattern '{pattern}'")
        if not wildcard and ("*" in seg or "#" in seg):
            raise BrokerError(f"invalid pattern '{pattern}'")
        if seg == "#" and i != last:
            raise BrokerError(f"invalid pattern '{pattern}'")
    return segs


def segments_match(pattern, topic, pi, ti):
    if pi == len(pattern):
        return ti == len(topic)
    if pattern[pi] == "#":
        return True
    if ti == len(topic):
        return False
    if pattern[pi] == "*" or pattern[pi] == topic[ti]:
        return segments_match(pattern, topic, pi + 1, ti + 1)
    return False


def topic_matches(pattern, topic):
    return segments_match(pattern.split("."), topic.split("."), 0, 0)


class Subscriber:
    def __init__(self, name, capacity):
        self.name = name
        self.capacity = capacity
        self.patterns = []
        self.mailbox = deque()
        self.accepted = 0
        self.dropped = 0

    def wants(self, topic):
        return any(topic_matches(p, topic) for p in self.patterns)

    def deliver(self, msg):
        evicted = None
        if len(self.mailbox) >= self.capacity:
            evicted = self.mailbox.popleft()
            self.dropped += 1
        self.mailbox.append(msg)
        self.accepted += 1
        return evicted


class Broker:
    def __init__(self):
        self.clients = []
        self.retained = {}
        self.next_id = 1
        self.published = 0
        self.deliveries = 0
        self.evictions = 0

    def find(self, name):
        for i, c in enumerate(self.clients):
            if c.name == name:
                return i
        return None

    def lookup(self, name):
        i = self.find(name)
        if i is None:
            raise BrokerError(f"unknown client '{name}'")
        return i

    def hand_to(self, idx, msg, log):
        self.deliveries += 1
        old = self.clients[idx].deliver(msg)
        if old is not None:
            self.evictions += 1
            log.append(f"  {self.clients[idx].name} mailbox full, dropped #{old.id}")

    def connect(self, name, capacity):
        if self.find(name) is not None:
            raise BrokerError(f"client '{name}' is already connected")
        line = f"connected {name} (capacity {capacity})"
        self.clients.append(Subscriber(name, capacity))
        return [line]

    def subscribe(self, name, pattern):
        validate_pattern(pattern)
        idx = self.lookup(name)
        log = []
        if pattern in self.clients[idx].patterns:
            log.append(f"{name} already subscribed to {pattern}")
            return log
        topics = sorted(t for t in self.retained if topic_matches(pattern, t))
        log.append(f"{name} subscribed to {pattern} ({len(topics)} retained)")
        self.clients[idx].patterns.append(pattern)
        for t in topics:
            self.hand_to(idx, self.retained[t].copy_as(True), log)
        return log

    def unsubscribe(self, name, pattern):
        idx = self.lookup(name)
        pats = self.clients[idx].patterns
        if pattern in pats:
            pats.remove(pattern)
            return [f"{name} unsubscribed from {pattern}"]
        raise BrokerError(f"client '{name}' is not subscribed to '{pattern}'")

    def publish(self, topic, payload, retain):
        validate_topic(topic)
        if retain and payload == "":
            had = self.retained.pop(topic, None) is not None
            return [f"cleared retained {topic} (had value: {'true' if had else 'false'})"]
        targets = [i for i, c in enumerate(self.clients) if c.wants(topic)]
        msg = Message(self.next_id, topic, payload, retain)
        self.next_id += 1
        self.published += 1
        kind = "pubr" if retain else "pub"
        log = [f"{kind} #{msg.id} {msg.topic} -> {len(targets)} subscriber(s)"]
        for i in targets:
            self.hand_to(i, msg.copy_as(False), log)
        if retain:
            self.retained[msg.topic] = msg
        return log

    def disconnect(self, name):
        idx = self.lookup(name)
        gone = self.clients.pop(idx)
        return [f"disconnected {name}, discarding {len(gone.mailbox)} pending message(s)"]

    def stats(self):
        return (f"stats: clients={len(self.clients)} published={self.published} "
                f"deliveries={self.deliveries} evictions={self.evictions} "
                f"retained={len(self.retained)}")

    def execute(self, line):
        cmd = parse_command(line)
        kind = cmd[0]
        if kind == "connect":
            return self.connect(cmd[1], cmd[2])
        if kind == "subscribe":
            return self.subscribe(cmd[1], cmd[2])
        if kind == "unsubscribe":
            return self.unsubscribe(cmd[1], cmd[2])
        if kind == "publish":
            return self.publish(cmd[1], cmd[2], cmd[3])
        if kind == "disconnect":
            return self.disconnect(cmd[1])
        return [self.stats()]


def parse_i64(s):
    try:
        return int(s)
    except ValueError:
        return None


def parse_command(line):
    parts = line.split(" ")
    n = len(parts)
    verb = parts[0]
    if verb == "connect" and n == 3:
        cap = parse_i64(parts[2])
        if cap is not None and cap > 0:
            return ("connect", parts[1], cap)
        raise BrokerError(f"invalid mailbox capacity '{parts[2]}'")
    if verb == "subscribe" and n == 3:
        return ("subscribe", parts[1], parts[2])
    if verb == "unsubscribe" and n == 3:
        return ("unsubscribe", parts[1], parts[2])
    if verb == "pub" and n >= 3:
        return ("publish", parts[1], " ".join(parts[2:]), False)
    if verb == "pubr" and n >= 2:
        return ("publish", parts[1], " ".join(parts[2:]), True)
    if verb == "disconnect" and n == 2:
        return ("disconnect", parts[1])
    if verb == "stats" and n == 1:
        return ("stats",)
    raise BrokerError(f"cannot parse command '{line}'")


def report(broker):
    print("== mailboxes ==")
    for c in broker.clients:
        pats = ", ".join(c.patterns)
        print(f"{c.name} cap={c.capacity} accepted={c.accepted} dropped={c.dropped} patterns=[{pats}]")
        if not c.mailbox:
            print("  (empty)")
        for m in c.mailbox:
            print(f"  {m.render()}")


SCRIPT = [
    "connect alice 3",
    "connect bob 2",
    "connect carol 4",
    "connect alice 5",
    "connect dave zero",
    "pubr sensors.kitchen.temp 21.5",
    "pubr sensors.garage.temp 14.0",
    "pub sensors.kitchen.humidity 40%",
    "subscribe alice sensors.*.temp",
    "subscribe bob sensors.#",
    "subscribe carol alerts.#",
    "subscribe carol sensors.kitchen.*",
    "subscribe carol alerts.#",
    "subscribe dave alerts.#",
    "subscribe bob sensors.#.temp",
    "pub sensors.kitchen.temp 22.0",
    "pub sensors.garage.temp 13.5",
    "pub alerts fire drill at noon",
    "pub alerts.security.door front door opened",
    "pub sensors..temp 1",
    "pub sensors.attic.temp 30.1",
    "pub sensors.attic.humidity 55%",
    "unsubscribe alice sensors.*.temp",
    "unsubscribe alice alerts.#",
    "subscribe alice alerts.*.door",
    "pub alerts.security.door back door opened",
    "pubr alerts.status armed",
    "subscribe alice alerts.status",
    "pubr sensors.kitchen.temp",
    "subscribe bob sensors.kitchen.temp",
    "stats",
    "disconnect bob",
    "pub sensors.kitchen.temp 23.0",
    "frobnicate now",
    "connect erin 1",
    "subscribe erin #",
    "pub ops.deploy v1.2.3 shipped",
    "pub ops.deploy v1.2.4 shipped",
    "stats",
]


def main():
    broker = Broker()
    step = 0
    errors = 0
    for raw in SCRIPT:
        line = raw.strip()
        if line == "":
            continue
        step += 1
        try:
            log = broker.execute(line)
        except BrokerError as e:
            errors += 1
            print(f"[{step}] error: {e}")
            continue
        for entry in log:
            print(f"[{step}] {entry}")
    print(f"{step} commands, {errors} rejected")
    report(broker)


if __name__ == "__main__":
    main()
