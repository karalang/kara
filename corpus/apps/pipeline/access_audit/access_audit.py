#!/usr/bin/env python3
"""Mirror of access_audit.kara: the same logs, parse and reports.

Run as: python3 -I -B access_audit.py > expected.out
"""
import os
import tempfile

SERVERS = ['web1', 'web2', 'web3', 'web4']
LINES_PER_SERVER = 8000
PATHS = ['/', '/login', '/logout', '/search', '/cart', '/checkout',
         '/api/items/{id}', '/api/users/{id}', '/api/orders/{id}', '/static/app.js',
         '/static/app.css', '/health']


class Lcg:
    def __init__(self, seed):
        self.state = seed

    def next(self, n):
        self.state = (self.state * 1103515245 + 12345) % 2147483648
        return (self.state >> 8) % n


def generate(server_no, n):
    r = Lcg(server_no * 7919 + 17)
    out = []
    t = 0
    for i in range(n):
        t += r.next(4)
        ip = '10.%d.%d.%d' % (r.next(3), r.next(8), r.next(40) + 1)
        roll = r.next(100)
        method = 'GET' if roll < 80 else ('POST' if roll < 95 else 'DELETE')
        route = PATHS[r.next(len(PATHS))]
        path = route.replace('{id}', str(r.next(500))) if '{id}' in route else route
        roll = r.next(100)
        if roll < 2:
            status = 500
        elif roll < 9:
            status = 404
        elif roll < 12:
            status = 301
        elif method == 'POST':
            status = 201
        else:
            status = 200
        size = r.next(20000) + 100 if status < 300 else r.next(400) + 50
        lat = r.next(120) + r.next(120) + 3
        if status == 500:
            lat += 900
        if path.startswith('/static/'):
            lat = lat // 10 + 1
        out.append('%s - - [t=%06d] "%s %s HTTP/1.1" %d %d %d\n' % (ip, t, method, path, status, size, lat))
    return ''.join(out)


class Hit:
    __slots__ = ('server', 'ip', 't', 'method', 'path', 'status', 'size', 'lat')


class ParseError(Exception):
    pass


def parse_line(server, line):
    # ip - - [t=NNNNNN] "METHOD PATH HTTP/1.1" STATUS SIZE LAT
    lb = line.find('[')
    rb = line.find(']')
    q1 = line.find('"')
    q2 = line.rfind('"')
    if lb < 0 or rb < lb or q1 < rb or q2 <= q1:
        raise ParseError('bad line: ' + line)
    h = Hit()
    h.server = server
    h.ip = line[:lb].split(' ')[0]
    h.t = int(line[lb + 3:rb])
    req = line[q1 + 1:q2].split(' ')
    if len(req) != 3:
        raise ParseError('bad request: ' + line)
    h.method, h.path = req[0], req[1]
    tail = line[q2 + 1:].strip().split(' ')
    if len(tail) != 3:
        raise ParseError('bad tail: ' + line)
    h.status, h.size, h.lat = int(tail[0]), int(tail[1]), int(tail[2])
    return h


def parse(server, text):
    return [parse_line(server, ln) for ln in text.split('\n') if ln]


def route_of(path):
    parts = path.split('/')
    out = []
    for p in parts:
        out.append('{id}' if p and p.isdigit() else p)
    return '/'.join(out)


def status_report(hits):
    counts = {}
    for h in hits:
        counts[h.status] = counts.get(h.status, 0) + 1
    errors = sum(v for k, v in counts.items() if k >= 500)
    lines = ['status codes:']
    for k in sorted(counts):
        lines.append('  %d: %d' % (k, counts[k]))
    lines.append('  error rate: %d.%02d%%' % (errors * 10000 // len(hits) // 100, errors * 10000 // len(hits) % 100))
    return '\n'.join(lines)


def top_routes(hits, n):
    counts = {}
    for h in hits:
        r = route_of(h.path)
        counts[r] = counts.get(r, 0) + 1
    ranked = sorted(counts.items(), key=lambda kv: (-kv[1], kv[0]))
    lines = ['top routes:']
    for r, c in ranked[:n]:
        lines.append('  %s %d' % (r, c))
    return '\n'.join(lines)


def pct(sorted_vals, p):
    i = (len(sorted_vals) * p + 99) // 100 - 1
    return sorted_vals[max(0, i)]


def latency_report(hits):
    by = {}
    for h in hits:
        by.setdefault(h.method, []).append(h.lat)
    lines = ['latency (ms):']
    for m in sorted(by):
        v = sorted(by[m])
        lines.append('  %s n=%d p50=%d p90=%d p99=%d max=%d' % (m, len(v), pct(v, 50), pct(v, 90), pct(v, 99), v[-1]))
    return '\n'.join(lines)


def client_report(hits, n):
    sent = {}
    for h in hits:
        sent[h.ip] = sent.get(h.ip, 0) + h.size
    ranked = sorted(sent.items(), key=lambda kv: (-kv[1], kv[0]))
    lines = ['clients: %d unique' % len(sent)]
    for ip, b in ranked[:n]:
        lines.append('  %s %d bytes' % (ip, b))
    return '\n'.join(lines)


def busiest_minutes(hits, n):
    per = {}
    for h in hits:
        key = (h.server, h.t // 60)
        per[key] = per.get(key, 0) + 1
    ranked = sorted(per.items(), key=lambda kv: (-kv[1], kv[0]))
    lines = ['busiest minutes:']
    for (s, m), c in ranked[:n]:
        lines.append('  %s minute %d: %d requests' % (s, m, c))
    return '\n'.join(lines)


def main():
    work = tempfile.mkdtemp()
    paths = [os.path.join(work, 'access_%s.log' % s) for s in SERVERS]
    for i, p in enumerate(paths):
        with open(p, 'w') as f:
            f.write(generate(i + 1, LINES_PER_SERVER))
    texts = []
    for p in paths:
        with open(p) as f:
            texts.append(f.read())
    logs = [parse(s, t) for s, t in zip(SERVERS, texts)]
    for s, log in zip(SERVERS, logs):
        print('%s: %d requests' % (s, len(log)))
    hits = [h for log in logs for h in log]
    print(status_report(hits))
    print(top_routes(hits, 6))
    print(latency_report(hits))
    print(client_report(hits, 3))
    print(busiest_minutes(hits, 3))
    for p in paths:
        os.remove(p)
    os.rmdir(work)


if __name__ == '__main__':
    main()
