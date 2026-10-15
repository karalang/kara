"""Mirror of jwt_auth.kara: an HTTP login service that issues and verifies
HS256 JSON Web Tokens, over a real TCP socket. The Kara writes SHA-256,
HMAC, PBKDF2 and base64url by hand; this mirror uses hashlib, hmac and
base64, which makes it an independent oracle for every byte of crypto."""
import base64
import hashlib
import hmac
import json
import socket

PORT = 47744
NOW = 1760000000
LEEWAY = 30
TTL = 900
ISSUER = "kara-auth"
SECRET = b"correct horse battery staple"
ITERATIONS = 100


def hexs(b):
    return b.hex()


def b64url(b):
    return base64.urlsafe_b64encode(b).rstrip(b"=").decode()


class Bad(Exception):
    pass


def unb64url(s):
    for c in s:
        if not (c.isascii() and (c.isalnum() or c in "-_")):
            raise Bad("malformed token: bad base64url")
    if len(s) % 4 == 1:
        raise Bad("malformed token: bad base64url")
    return base64.urlsafe_b64decode(s + "=" * (-len(s) % 4))


def pbkdf2(password, salt, iterations):
    return hashlib.pbkdf2_hmac("sha256", password, salt, iterations, 32)


def jq(s):
    return '"' + s.replace("\\", "\\\\").replace('"', '\\"') + '"'


def claims_json(sub, roles, iat, exp, nbf):
    rs = ",".join(jq(r) for r in roles)
    out = f'{{"iss":{jq(ISSUER)},"sub":{jq(sub)},"roles":[{rs}],"iat":{iat},"exp":{exp}'
    if nbf is not None:
        out += f',"nbf":{nbf}'
    return out + "}"


def sign(header_json, claims, key):
    h = b64url(header_json.encode())
    p = b64url(claims.encode())
    sig = hmac.new(key, f"{h}.{p}".encode(), hashlib.sha256).digest()
    return f"{h}.{p}.{b64url(sig)}"


HS256 = '{"alg":"HS256","typ":"JWT"}'


def issue(sub, roles):
    return sign(HS256, claims_json(sub, roles, NOW, NOW + TTL, None), SECRET)


def parse_obj(b, what):
    try:
        v = json.loads(b.decode())
    except (ValueError, UnicodeDecodeError):
        raise Bad(f"malformed token: {what} is not a JSON object")
    if not isinstance(v, dict):
        raise Bad(f"malformed token: {what} is not a JSON object")
    return v


def verify(token):
    parts = token.split(".")
    if len(parts) != 3:
        raise Bad(f"malformed token: {len(parts)} segments")
    header = parse_obj(unb64url(parts[0]), "header")
    alg = header.get("alg")
    if alg != "HS256":
        raise Bad(f"unsupported alg {alg if isinstance(alg, str) else '?'}")
    want = hmac.new(SECRET, f"{parts[0]}.{parts[1]}".encode(), hashlib.sha256).digest()
    got = unb64url(parts[2])
    if not hmac.compare_digest(want, got):
        raise Bad("bad signature")
    c = parse_obj(unb64url(parts[1]), "claims")
    if c.get("iss") != ISSUER:
        raise Bad("wrong issuer")
    exp = c.get("exp")
    if not isinstance(exp, int):
        raise Bad("no expiry")
    if NOW > exp + LEEWAY:
        raise Bad(f"token expired {NOW - exp}s ago")
    nbf = c.get("nbf")
    if isinstance(nbf, int) and nbf > NOW + LEEWAY:
        raise Bad(f"token not valid for another {nbf - NOW}s")
    return c


USERS = {}


def add_user(name, password, salt, roles):
    USERS[name] = (salt, pbkdf2(password.encode(), salt.encode(), ITERATIONS), roles)


def form(body):
    out = {}
    for pair in body.split("&"):
        if "=" in pair:
            k, v = pair.split("=", 1)
            out[k] = v
    return out


def response(status, body, extra=()):
    reason = {200: "OK", 401: "Unauthorized", 403: "Forbidden", 404: "Not Found", 405: "Method Not Allowed",
              400: "Bad Request"}[status]
    head = f"HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {len(body)}\r\n"
    for h in extra:
        head += h + "\r\n"
    return (head + "\r\n" + body).encode()


def err(status, msg, extra=()):
    return response(status, f'{{"error":{jq(msg)}}}', extra)


def bearer(headers):
    a = headers.get("authorization")
    if a is None or not a.startswith("Bearer "):
        return None
    return a[7:]


def handle(method, path, headers, body):
    if path == "/login":
        if method != "POST":
            return err(405, "use POST", ["Allow: POST"])
        f = form(body)
        u = USERS.get(f.get("user", ""))
        ok = False
        if u is not None:
            ok = hmac.compare_digest(pbkdf2(f.get("password", "").encode(), u[0].encode(), ITERATIONS), u[1])
        if not ok:
            return err(401, "invalid credentials")
        return response(200, f'{{"token":{jq(issue(f["user"], u[2]))},"expires_in":{TTL}}}')
    if path in ("/me", "/admin"):
        if method != "GET":
            return err(405, "use GET", ["Allow: GET"])
        tok = bearer(headers)
        if tok is None:
            return err(401, "missing bearer token", ['WWW-Authenticate: Bearer realm="kara"'])
        try:
            c = verify(tok)
        except Bad as e:
            return err(401, str(e), [f'WWW-Authenticate: Bearer error="invalid_token"'])
        roles = c.get("roles", [])
        if path == "/me":
            return response(200, f'{{"sub":{jq(c["sub"])},"roles":[{",".join(jq(r) for r in roles)}]}}')
        if "admin" not in roles:
            return err(403, "needs role admin")
        return response(200, f'{{"ok":{jq("welcome, admin " + c["sub"])}}}')
    return err(404, f"no route {path}")


def serve(data):
    out = b""
    at = 0
    n = 0
    while at < len(data):
        end = data.find(b"\r\n\r\n", at)
        if end < 0:
            break
        head = data[at:end].decode().split("\r\n")
        method, path, _ = head[0].split(" ")
        headers = {}
        for h in head[1:]:
            k, v = h.split(":", 1)
            headers[k.strip().lower()] = v.strip()
        length = int(headers.get("content-length", "0"))
        body = data[end + 4:end + 4 + length].decode()
        at = end + 4 + length
        out += handle(method, path, headers, body)
        n += 1
    return out, n


def request(method, path, token=None, body=""):
    r = f"{method} {path} HTTP/1.1\r\nHost: localhost\r\n"
    if token is not None:
        r += f"Authorization: Bearer {token}\r\n"
    if body:
        r += f"Content-Type: application/x-www-form-urlencoded\r\nContent-Length: {len(body)}\r\n"
    return r + "\r\n" + body


def session(listener, label, reqs):
    print(f"== {label}")
    c = socket.create_connection(("127.0.0.1", PORT))
    c.sendall("".join(reqs).encode())
    c.shutdown(socket.SHUT_WR)
    conn, _ = listener.accept()
    data = b""
    while True:
        chunk = conn.recv(64)
        if not chunk:
            break
        data += chunk
    out, n = serve(data)
    print(f"  server: {n} requests")
    conn.sendall(out)
    conn.close()
    got = b""
    while True:
        chunk = c.recv(64)
        if not chunk:
            break
        got += chunk
    c.close()
    text = got.decode()
    replies = []
    at = 0
    while at < len(text):
        end = text.find("\r\n\r\n", at)
        lines = text[at:end].split("\r\n")
        length = 0
        shown = []
        for h in lines[1:]:
            k, v = h.split(":", 1)
            if k == "Content-Length":
                length = int(v)
            if k in ("WWW-Authenticate", "Allow"):
                shown.append(f"{k}: {v.strip()}")
        body = text[end + 4:end + 4 + length]
        at = end + 4 + length
        replies.append((lines[0], shown, body))
    return replies


def show(replies):
    for status, shown, body in replies:
        print(f"  {status}")
        for h in shown:
            print(f"    {h}")
        print(f"    {body}")


def token_of(body):
    k = body.find('"token":"')
    return body[k + 9:body.find('"', k + 9)]


def main():
    print(f"sha256('') = {hexs(hashlib.sha256(b'').digest())}")
    print(f"sha256('abc') = {hexs(hashlib.sha256(b'abc').digest())}")
    two = b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"
    print(f"sha256(56 bytes) = {hexs(hashlib.sha256(two).digest())}")
    print(f"sha256('a' * 1000) = {hexs(hashlib.sha256(b'a' * 1000).digest())}")
    print(f"hmac rfc4231 #2 = {hexs(hmac.new(b'Jefe', b'what do ya want for nothing?', hashlib.sha256).digest())}")
    long_key = bytes([0xAA] * 131)
    print(f"hmac rfc4231 #6 = {hexs(hmac.new(long_key, b'Test Using Larger Than Block-Size Key - Hash Key First', hashlib.sha256).digest())}")
    print(f"pbkdf2 c=1 = {hexs(pbkdf2(b'password', b'salt', 1))}")
    print(f"pbkdf2 c=2 = {hexs(pbkdf2(b'password', b'salt', 2))}")
    for s in [b"", b"f", b"fo", b"foo", bytes([0xFB, 0xFF, 0xBF])]:
        print(f"b64url({s.hex()}) = [{b64url(s)}]")

    add_user("alice", "wonderland", "s4lt-a", ["admin", "dev"])
    add_user("bob", "builder", "s4lt-b", ["dev"])
    print(f"users: alice key {hexs(USERS['alice'][1])[:16]}..., bob key {hexs(USERS['bob'][1])[:16]}...")

    listener = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    listener.bind(("127.0.0.1", PORT))
    listener.listen(4)

    r = session(listener, "logins", [
        request("POST", "/login", body="user=alice&password=wonderland"),
        request("POST", "/login", body="user=alice&password=guess"),
        request("POST", "/login", body="user=mallory&password=x"),
        request("POST", "/login", body="user=bob&password=builder"),
        request("GET", "/login"),
    ])
    show(r)
    alice = token_of(r[0][2])
    bob = token_of(r[3][2])
    parts = alice.split(".")
    print(f"  alice token: {len(alice)} chars, header {base64.urlsafe_b64decode(parts[0] + '==').decode()}")
    print(f"  claims {base64.urlsafe_b64decode(parts[1] + '=' * (-len(parts[1]) % 4)).decode()}")
    print(f"  signature {parts[2]}")

    h, p, _ = alice.split(".")
    forged = claims_json("alice", ["admin"], NOW, NOW + TTL, None).replace("alice", "mallory")
    tampered = f"{h}.{b64url(forged.encode())}.{alice.split('.')[2]}"
    none = f"{b64url(b'{\"alg\":\"none\",\"typ\":\"JWT\"}')}.{p}."
    other = sign(HS256, claims_json("alice", ["admin"], NOW, NOW + TTL, None), b"some other key")
    old = sign(HS256, claims_json("alice", ["dev"], NOW - 1020, NOW - 120, None), SECRET)
    grace = sign(HS256, claims_json("alice", ["dev"], NOW - 910, NOW - 10, None), SECRET)
    early = sign(HS256, claims_json("alice", ["dev"], NOW, NOW + TTL, NOW + 600), SECRET)
    foreign = sign(HS256, claims_json("alice", ["dev"], NOW, NOW + TTL, None).replace(ISSUER, "evil"), SECRET)
    r = session(listener, "protected routes", [
        request("GET", "/me", alice),
        request("GET", "/admin", alice),
        request("GET", "/me", bob),
        request("GET", "/admin", bob),
        request("GET", "/me"),
        request("PUT", "/me", alice),
        request("GET", "/nope", alice),
    ])
    show(r)
    r = session(listener, "attacks and edge cases", [
        request("GET", "/admin", tampered),
        request("GET", "/admin", none),
        request("GET", "/admin", other),
        request("GET", "/me", old),
        request("GET", "/me", grace),
        request("GET", "/me", early),
        request("GET", "/me", foreign),
        request("GET", "/me", "abc.def"),
        request("GET", "/me", "e!x.y.z"),
        request("GET", "/me", "a.b.c.d"),
    ])
    show(r)
    listener.close()


main()
