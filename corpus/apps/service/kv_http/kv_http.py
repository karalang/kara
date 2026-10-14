# Python mirror of source.kara: same protocol, same data, same output.
import json
import socket

def dumps(o):
    return json.dumps(o, separators=(",", ":"))

REASON = {200: "OK", 201: "Created", 204: "No Content", 400: "Bad Request",
          404: "Not Found", 405: "Method Not Allowed", 411: "Length Required"}

def error_reply(status, msg):
    return (status, dumps({"error": msg, "status": status}))

def header_value(req, name):
    for k, v in req["headers"]:
        if k.lower() == name.lower():
            return v
    return None

class ParseError(Exception):
    pass

def parse_requests(raw):
    out = []
    rest = raw
    while len(rest) > 0:
        head_end = rest.find("\r\n\r\n")
        if head_end < 0:
            raise ParseError(f"truncated head after {len(out)} requests")
        lines = rest[:head_end].split("\r\n")
        first = lines[0].split(" ")
        if len(first) != 3 or not first[2].startswith("HTTP/1."):
            raise ParseError(f"bad request line [{lines[0]}]")
        headers = []
        for l in lines[1:]:
            c = l.find(":")
            if c < 0:
                raise ParseError(f"bad header [{l}]")
            headers.append((l[:c].strip(), l[c + 1:].strip()))
        req = {"method": first[0], "path": first[1], "headers": headers, "body": ""}
        body_start = head_end + 4
        v = header_value(req, "content-length")
        if v is None:
            n = 0
        else:
            try:
                n = int(v)
            except ValueError:
                raise ParseError(f"bad content-length [{v}]")
        if body_start + n > len(rest):
            raise ParseError(f"body short by {body_start + n - len(rest)} bytes")
        req["body"] = rest[body_start:body_start + n]
        rest = rest[body_start + n:]
        out.append(req)
    return out

def key_of(path):
    if path.startswith("/kv/") and len(path) > 4:
        return path[4:]
    return None

def handle(store, req):
    st = store["stats"]
    st["requests"] += 1
    st["bytes_in"] += len(req["body"].encode())
    items = store["items"]
    if req["path"] == "/kv":
        if req["method"] != "GET":
            return error_reply(405, "list is GET only")
        keys = sorted(items)
        return (200, dumps({"count": len(keys), "keys": keys}))
    if req["path"] == "/stats":
        return (200, dumps(st))
    key = key_of(req["path"])
    if key is None:
        return error_reply(404, f"no route {req['path']}")
    m = req["method"]
    if m == "GET":
        if key in items:
            st["hits"] += 1
            return (200, dumps(items[key]))
        st["misses"] += 1
        return error_reply(404, f"no key {key}")
    if m == "PUT":
        if header_value(req, "content-length") is None:
            return error_reply(411, "PUT needs a body")
        try:
            b = json.loads(req["body"])
            if not isinstance(b.get("value"), str) or not isinstance(b.get("tags"), list):
                raise ValueError
        except ValueError:
            return error_reply(400, "bad json")
        version = items[key]["version"] + 1 if key in items else 1
        item = {"key": key, "value": b["value"], "tags": b["tags"], "version": version}
        items[key] = item
        st["writes"] += 1
        return (201 if version == 1 else 200, dumps(item))
    if m == "DELETE":
        if key in items:
            del items[key]
            st["writes"] += 1
            return (204, "")
        st["misses"] += 1
        return error_reply(404, f"no key {key}")
    return error_reply(405, f"method {m}")

def render(r):
    status, body = r
    s = f"HTTP/1.1 {status} {REASON.get(status, 'Unknown')}\r\n"
    if body:
        s += "Content-Type: application/json\r\n"
    s += f"Content-Length: {len(body.encode())}\r\n\r\n" + body
    return s

def read_all(conn):
    data = b""
    while True:
        chunk = conn.recv(64)
        if not chunk:
            break
        data += chunk
    return data.decode()

def request(method, path, body=None):
    s = f"{method} {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nUser-Agent: kv-client\r\n"
    if body is not None:
        s += f"Content-Length: {len(body.encode())}\r\n\r\n" + body
    else:
        s += "\r\n"
    return s

def exchange(listener, addr, store, label, batch):
    print(f"== {label}")
    client = socket.create_connection(addr)
    client.sendall(batch.encode())
    client.shutdown(socket.SHUT_WR)
    conn, _ = listener.accept()
    out = ""
    try:
        for r in parse_requests(read_all(conn)):
            out += render(handle(store, r))
    except ParseError as e:
        out += render(error_reply(400, str(e)))
    conn.sendall(out.encode())
    conn.close()
    print_responses(read_all(client))
    client.close()

def print_responses(resp):
    at = 0
    while at < len(resp):
        rest = resp[at:]
        head_end = rest.find("\r\n\r\n")
        if head_end < 0:
            print(f"  trailing [{rest}]")
            return
        n = 0
        status = ""
        for line in rest[:head_end].split("\r\n"):
            if line.startswith("HTTP/1.1 "):
                status = line
            elif line.lower().startswith("content-length:"):
                try:
                    n = int(line[15:].strip())
                except ValueError:
                    n = 0
        body = rest[head_end + 4:head_end + 4 + n]
        print(f"  {status} (empty)" if n == 0 else f"  {status} {body}")
        at += head_end + 4 + n

def put(key, value, tags):
    return request("PUT", f"/kv/{key}", dumps({"value": value, "tags": tags}))

def main():
    addr = ("127.0.0.1", 47731)
    listener = socket.socket()
    listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    listener.bind(addr)
    listener.listen(16)
    store = {"items": {}, "stats": {"requests": 0, "hits": 0, "misses": 0, "writes": 0, "bytes_in": 0}}
    exchange(listener, addr, store, "create", put("alpha", "first", ["a"]))
    batch = put("beta", "second", ["b", "hot"]) + put("gamma", 'third "quoted"', []) \
        + put("alpha", "first, again", ["a", "v2"])
    exchange(listener, addr, store, "pipelined writes", batch)
    reads = request("GET", "/kv/alpha") + request("GET", "/kv/missing") + request("GET", "/kv")
    exchange(listener, addr, store, "reads", reads)
    errs = request("PUT", "/kv/bad", '{"value": 3}') + request("POST", "/kv/alpha", "{}") \
        + request("GET", "/nowhere") + request("PUT", "/kv/nobody")
    exchange(listener, addr, store, "errors", errs)
    dels = request("DELETE", "/kv/beta") + request("DELETE", "/kv/beta") + request("GET", "/kv")
    exchange(listener, addr, store, "deletes", dels)
    exchange(listener, addr, store, "garbage", "HELLO\r\n\r\n")
    short = request("PUT", "/kv/delta", '{"value":"x","tags":[]}')
    exchange(listener, addr, store, "short body", short[:-5])
    exchange(listener, addr, store, "stats", request("GET", "/stats"))

main()
