"""Oracle for sigv4_store.kara: an S3-style bucket store on loopback HTTP
whose every request is signed with AWS Signature Version 4, by header or by
presigned URL, and the clients that sign them. Object bodies are bytes."""

import hashlib
import hmac
import http.server
import threading
import time
import urllib.error
import urllib.request

ADDR = ("127.0.0.1", 47786)
HOST = f"{ADDR[0]}:{ADDR[1]}"
BASE = f"http://{HOST}"
REGION = "kara-local-1"
SERVICE = "s3"
ALGORITHM = "AWS4-HMAC-SHA256"
# The server's clock is fixed so that every run prints the same answers.
NOW = "20261123T120000Z"
MAX_SKEW = 15 * 60
MAX_EXPIRES = 7 * 24 * 3600
EMPTY_SHA256 = hashlib.sha256(b"").hexdigest()
UNSIGNED = "UNSIGNED-PAYLOAD"

# Obviously fake credentials: an access key id, its secret, and what it may do.
KEYS = {
    "AKIDKARAWRITER01": ("not-a-real-secret/writer", True),
    "AKIDKARAREADER01": ("not-a-real-secret/reader", False),
}


# ---- signing


def hmac256(key, msg):
    return hmac.new(key, msg, hashlib.sha256).digest()


def uri_encode(s, keep_slash):
    out = []
    for b in s.encode():
        c = chr(b)
        if c.isascii() and (c.isalnum() or c in "-._~") or (keep_slash and c == "/"):
            out.append(c)
        else:
            out.append(f"%{b:02X}")
    return "".join(out)


def uri_decode(s):
    out = bytearray()
    i = 0
    raw = s.encode()
    while i < len(raw):
        if raw[i] == ord("%"):
            h = raw[i + 1 : i + 3]
            if len(h) != 2 or not all(chr(x) in "0123456789abcdefABCDEF" for x in h):
                return None
            out.append(int(h, 16))
            i += 3
        else:
            out.append(raw[i])
            i += 1
    try:
        return out.decode()
    except UnicodeDecodeError:
        return None


def canonical_query(pairs):
    enc = sorted((uri_encode(k, False), uri_encode(v, False)) for k, v in pairs)
    return "&".join(f"{k}={v}" for k, v in enc)


def signing_key(secret, date, region):
    k = hmac256(("AWS4" + secret).encode(), date.encode())
    k = hmac256(k, region.encode())
    k = hmac256(k, SERVICE.encode())
    return hmac256(k, b"aws4_request")


def signature(secret, method, path, query, headers, signed, payload, stamp, region):
    """`path` is the decoded path; `headers` maps lower-case names to values."""
    creq = "\n".join(
        [
            method,
            uri_encode(path, True),
            canonical_query(query),
            "".join(f"{h}:{' '.join(headers[h].split())}\n" for h in signed),
            ";".join(signed),
            payload,
        ]
    )
    scope = f"{stamp[:8]}/{region}/{SERVICE}/aws4_request"
    sts = "\n".join([ALGORITHM, stamp, scope, hashlib.sha256(creq.encode()).hexdigest()])
    return hmac.new(signing_key(secret, stamp[:8], region), sts.encode(), hashlib.sha256).hexdigest()


def seconds(stamp):
    """Seconds since 1970 of a `YYYYMMDDTHHMMSSZ` stamp, or None."""
    if len(stamp) != 16 or stamp[8] != "T" or stamp[15] != "Z":
        return None
    digits = stamp[:8] + stamp[9:15]
    if not digits.isdigit():
        return None
    y, m, d = int(stamp[0:4]), int(stamp[4:6]), int(stamp[6:8])
    hh, mm, ss = int(stamp[9:11]), int(stamp[11:13]), int(stamp[13:15])
    if not (1 <= m <= 12 and 1 <= d <= 31 and hh < 24 and mm < 60 and ss < 60):
        return None
    # days from civil (Howard Hinnant)
    y -= m <= 2
    era = y // 400
    yoe = y - era * 400
    doy = (153 * (m + (-3 if m > 2 else 9)) + 2) // 5 + d - 1
    doe = yoe * 365 + yoe // 4 - yoe // 100 + doy
    days = era * 146097 + doe - 719468
    return days * 86400 + hh * 3600 + mm * 60 + ss


# ---- the store


def xml_escape(s):
    return s.replace("&", "&amp;").replace("<", "&lt;").replace(">", "&gt;").replace('"', "&quot;")


def error(status, code, message):
    body = f'<?xml version="1.0" encoding="UTF-8"?>\n<Error><Code>{code}</Code><Message>{xml_escape(message)}</Message></Error>'
    return status, body.encode(), [("Content-Type", "application/xml")]


def valid_bucket(name):
    return 3 <= len(name) <= 63 and all(c.islower() or c.isdigit() or c == "-" for c in name) and name[0] != "-" and name[-1] != "-"


class AuthError(Exception):
    def __init__(self, status, code, message):
        self.answer = error(status, code, message)


class Store:
    def __init__(self):
        self.buckets = {}
        self.lock = threading.Lock()
        self.counts = {}

    def count(self, what):
        self.counts[what] = self.counts.get(what, 0) + 1

    def authenticate(self, method, path, query, headers, body):
        """The access key id the request is signed by, or AuthError."""
        q = dict(query)
        if "X-Amz-Signature" in q:
            return self.presigned(method, path, query, headers)
        auth = headers.get("authorization")
        if auth is None:
            raise AuthError(403, "AccessDenied", "the request is not signed")
        if not auth.startswith(ALGORITHM + " "):
            raise AuthError(400, "AuthorizationHeaderMalformed", f"the algorithm must be {ALGORITHM}")
        parts = {}
        for item in auth[len(ALGORITHM) + 1 :].split(","):
            k, sep, v = item.strip().partition("=")
            if not sep:
                raise AuthError(400, "AuthorizationHeaderMalformed", f"'{item.strip()}' is not name=value")
            parts[k] = v
        for need in ("Credential", "SignedHeaders", "Signature"):
            if need not in parts:
                raise AuthError(400, "AuthorizationHeaderMalformed", f"missing {need}")
        stamp = headers.get("x-amz-date")
        if stamp is None:
            raise AuthError(403, "AccessDenied", "missing x-amz-date")
        akid, secret = self.check_credential(parts["Credential"], stamp)
        signed = parts["SignedHeaders"].split(";")
        if signed != sorted(signed) or "host" not in signed or "x-amz-date" not in signed:
            raise AuthError(400, "AuthorizationHeaderMalformed", "SignedHeaders must be sorted and name host and x-amz-date")
        for h in signed:
            if h not in headers:
                raise AuthError(400, "AuthorizationHeaderMalformed", f"signed header {h} is not in the request")
        self.check_skew(stamp)
        payload = headers.get("x-amz-content-sha256")
        if payload is None:
            raise AuthError(400, "InvalidRequest", "missing x-amz-content-sha256")
        want = signature(secret, method, path, query, headers, signed, payload, stamp, REGION)
        if not hmac.compare_digest(want, parts["Signature"]):
            raise AuthError(403, "SignatureDoesNotMatch", "the signature does not match")
        if payload != UNSIGNED and payload != hashlib.sha256(body).hexdigest():
            raise AuthError(400, "XAmzContentSHA256Mismatch", "the body's SHA-256 is not x-amz-content-sha256")
        return akid

    def check_credential(self, cred, stamp):
        fields = cred.split("/")
        if len(fields) != 5 or fields[4] != "aws4_request" or fields[3] != SERVICE:
            raise AuthError(400, "AuthorizationHeaderMalformed", f"bad credential scope '{cred}'")
        akid, date, region = fields[0], fields[1], fields[2]
        if akid not in KEYS:
            raise AuthError(403, "InvalidAccessKeyId", f"no access key {akid}")
        if region != REGION:
            raise AuthError(400, "AuthorizationHeaderMalformed", f"the region '{region}' is wrong; expecting '{REGION}'")
        if date != stamp[:8]:
            raise AuthError(400, "AuthorizationHeaderMalformed", f"the credential date {date} is not the date of x-amz-date")
        return akid, KEYS[akid][0]

    def check_skew(self, stamp):
        t = seconds(stamp)
        if t is None:
            raise AuthError(403, "AccessDenied", f"x-amz-date '{stamp}' is not YYYYMMDDTHHMMSSZ")
        if abs(t - seconds(NOW)) > MAX_SKEW:
            raise AuthError(403, "RequestTimeTooSkewed", f"{stamp} is more than 15 minutes from the server's time")

    def presigned(self, method, path, query, headers):
        q = dict(query)
        for need in ("X-Amz-Algorithm", "X-Amz-Credential", "X-Amz-Date", "X-Amz-Expires", "X-Amz-SignedHeaders"):
            if need not in q:
                raise AuthError(400, "AuthorizationQueryParametersError", f"missing {need}")
        if q["X-Amz-Algorithm"] != ALGORITHM:
            raise AuthError(400, "AuthorizationQueryParametersError", f"the algorithm must be {ALGORITHM}")
        stamp = q["X-Amz-Date"]
        akid, secret = self.check_credential(q["X-Amz-Credential"], stamp)
        t = seconds(stamp)
        if t is None:
            raise AuthError(403, "AccessDenied", f"X-Amz-Date '{stamp}' is not YYYYMMDDTHHMMSSZ")
        exp = q["X-Amz-Expires"]
        if not exp.isdigit() or not 1 <= int(exp) <= MAX_EXPIRES:
            raise AuthError(400, "AuthorizationQueryParametersError", "X-Amz-Expires must be 1 to 604800 seconds")
        if t > seconds(NOW) + MAX_SKEW:
            raise AuthError(403, "AccessDenied", "the request is signed in the future")
        if t + int(exp) < seconds(NOW):
            raise AuthError(403, "AccessDenied", "the request has expired")
        signed = q["X-Amz-SignedHeaders"].split(";")
        for h in signed:
            if h not in headers:
                raise AuthError(400, "AuthorizationQueryParametersError", f"signed header {h} is not in the request")
        rest = [(k, v) for k, v in query if k != "X-Amz-Signature"]
        want = signature(secret, method, path, rest, headers, signed, UNSIGNED, stamp, REGION)
        if not hmac.compare_digest(want, q["X-Amz-Signature"]):
            raise AuthError(403, "SignatureDoesNotMatch", "the signature does not match")
        return akid

    def handle(self, method, target, headers, body):
        raw_path, _, raw_query = target.partition("?")
        path = uri_decode(raw_path)
        query = []
        if raw_query:
            for item in raw_query.split("&"):
                k, _, v = item.partition("=")
                dk, dv = uri_decode(k), uri_decode(v)
                if dk is None or dv is None:
                    path = None
                    break
                query.append((dk, dv))
        if path is None:
            self.count("400")
            return error(400, "InvalidURI", "bad percent-encoding")
        if path == "/_stats" and method == "GET":
            line = " ".join(f"{k}={v}" for k, v in sorted(self.counts.items()))
            return 200, line.encode(), [("Content-Type", "text/plain")]
        try:
            akid = self.authenticate(method, path, query, headers, body)
        except AuthError as e:
            self.count(str(e.answer[0]))
            return e.answer
        answer = self.route(method, path, dict(query), headers, body, KEYS[akid][1])
        self.count(str(answer[0]))
        return answer

    def route(self, method, path, q, headers, body, may_write):
        bucket, _, key = path[1:].partition("/")
        if method in ("PUT", "DELETE") and not may_write:
            return error(403, "AccessDenied", f"{method} needs write access")
        if bucket == "":
            if method != "GET":
                return error(405, "MethodNotAllowed", f"{method} on the service")
            names = "".join(f"<Bucket><Name>{xml_escape(b)}</Name></Bucket>" for b in sorted(self.buckets))
            return 200, f'<?xml version="1.0" encoding="UTF-8"?>\n<ListAllMyBucketsResult><Buckets>{names}</Buckets></ListAllMyBucketsResult>'.encode(), [("Content-Type", "application/xml")]
        if key == "":
            if method == "PUT":
                if not valid_bucket(bucket):
                    return error(400, "InvalidBucketName", f"'{bucket}' is not a valid bucket name")
                if bucket in self.buckets:
                    return error(409, "BucketAlreadyOwnedByYou", f"{bucket} exists")
                self.buckets[bucket] = {}
                return 200, b"", [("Location", f"/{bucket}")]
            if method == "GET":
                if bucket not in self.buckets:
                    return error(404, "NoSuchBucket", f"no bucket {bucket}")
                return self.list(bucket, q)
            return error(405, "MethodNotAllowed", f"{method} on a bucket")
        objects = self.buckets.get(bucket)
        if objects is None:
            return error(404, "NoSuchBucket", f"no bucket {bucket}")
        if method == "PUT":
            etag = '"' + hashlib.sha256(body).hexdigest()[:32] + '"'
            ctype = headers.get("content-type", "application/octet-stream")
            objects[key] = (body, ctype, etag)
            return 200, b"", [("ETag", etag)]
        if method == "GET":
            if key not in objects:
                return error(404, "NoSuchKey", f"no key {key}")
            data, ctype, etag = objects[key]
            return 200, data, [("Content-Type", ctype), ("ETag", etag)]
        if method == "DELETE":
            objects.pop(key, None)
            return 204, b"", []
        return error(405, "MethodNotAllowed", f"{method} on an object")

    def list(self, bucket, q):
        if q.get("list-type") != "2":
            return error(400, "InvalidArgument", "only list-type=2 is served")
        prefix = q.get("prefix", "")
        delim = q.get("delimiter", "")
        mk = q.get("max-keys", "1000")
        if not mk.isdigit() or int(mk) > 1000:
            return error(400, "InvalidArgument", "max-keys must be 0 to 1000")
        max_keys = int(mk)
        after = ""
        if "continuation-token" in q:
            tok = q["continuation-token"]
            try:
                after = bytes.fromhex(tok).decode()
            except ValueError:
                return error(400, "InvalidArgument", "the continuation token is not valid")
        contents, prefixes = [], []
        last = None
        truncated = False
        for key in sorted(self.buckets[bucket]):
            if not key.startswith(prefix):
                continue
            entry = key
            rest = key[len(prefix):]
            if delim and delim in rest:
                entry = prefix + rest[: rest.index(delim) + len(delim)]
            if after and (entry <= after or (after.endswith(delim) and delim and entry.startswith(after))):
                continue
            if entry == last:
                continue
            if len(contents) + len(prefixes) == max_keys:
                truncated = True
                break
            last = entry
            if entry == key:
                data, _, etag = self.buckets[bucket][key]
                contents.append((key, len(data), etag))
            else:
                prefixes.append(entry)
        x = [f"<Name>{xml_escape(bucket)}</Name><Prefix>{xml_escape(prefix)}</Prefix>"]
        if delim:
            x.append(f"<Delimiter>{xml_escape(delim)}</Delimiter>")
        x.append(f"<MaxKeys>{max_keys}</MaxKeys><KeyCount>{len(contents) + len(prefixes)}</KeyCount>")
        x.append(f"<IsTruncated>{'true' if truncated else 'false'}</IsTruncated>")
        for key, size, etag in contents:
            x.append(f"<Contents><Key>{xml_escape(key)}</Key><Size>{size}</Size><ETag>{xml_escape(etag)}</ETag></Contents>")
        for p in prefixes:
            x.append(f"<CommonPrefixes><Prefix>{xml_escape(p)}</Prefix></CommonPrefixes>")
        if truncated:
            x.append(f"<NextContinuationToken>{last.encode().hex()}</NextContinuationToken>")
        body = '<?xml version="1.0" encoding="UTF-8"?>\n<ListBucketResult>' + "".join(x) + "</ListBucketResult>"
        return 200, body.encode(), [("Content-Type", "application/xml")]


STORE = Store()


class Handler(http.server.BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def serve(self):
        n = int(self.headers.get("Content-Length", "0"))
        body = self.rfile.read(n) if n else b""
        headers = {k.lower(): v for k, v in self.headers.items()}
        with STORE.lock:
            status, data, extra = STORE.handle(self.command, self.path, headers, body)
        self.send_response(status)
        names = {k.lower() for k, _ in extra}
        for k, v in extra:
            self.send_header(k, v)
        if "content-type" not in names and data:
            self.send_header("Content-Type", "application/octet-stream")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    do_GET = do_PUT = do_DELETE = do_POST = serve

    def log_message(self, *a):
        pass


# ---- the clients


def tag(xml, name):
    """Every `<name>..</name>` text in `xml`, unescaped, in order."""
    out, at = [], 0
    while True:
        i = xml.find(f"<{name}>", at)
        if i < 0:
            return out
        j = xml.find(f"</{name}>", i)
        text = xml[i + len(name) + 2 : j]
        out.append(text.replace("&quot;", '"').replace("&lt;", "<").replace("&gt;", ">").replace("&amp;", "&"))
        at = j


def describe(status, body):
    if status >= 400:
        text = body.decode()
        return f"{status} {tag(text, 'Code')[0]}: {tag(text, 'Message')[0]}"
    return str(status)


class Signer:
    def __init__(self, akid, secret, region=REGION):
        self.akid, self.secret, self.region = akid, secret, region

    def send(self, method, key_path, query=(), body=b"", ctype=None, stamp=NOW, payload=None, path_sent=None):
        """Sign and send; `key_path` is the decoded path. `path_sent` sends a
        different path from the one signed."""
        payload = payload or hashlib.sha256(body).hexdigest()
        headers = {"host": HOST, "x-amz-content-sha256": payload, "x-amz-date": stamp}
        signed = sorted(headers)
        sig = signature(self.secret, method, key_path, list(query), headers, signed, payload, stamp, self.region)
        cred = f"{self.akid}/{stamp[:8]}/{self.region}/{SERVICE}/aws4_request"
        auth = f"{ALGORITHM} Credential={cred}, SignedHeaders={';'.join(signed)}, Signature={sig}"
        send = {"Authorization": auth, "x-amz-content-sha256": payload, "x-amz-date": stamp}
        if ctype:
            send["Content-Type"] = ctype
        url = BASE + uri_encode(path_sent or key_path, True)
        if query:
            url += "?" + canonical_query(query)
        return fetch(method, url, send, body)

    def presign(self, key_path, expires, stamp=NOW):
        cred = f"{self.akid}/{stamp[:8]}/{self.region}/{SERVICE}/aws4_request"
        query = [
            ("X-Amz-Algorithm", ALGORITHM),
            ("X-Amz-Credential", cred),
            ("X-Amz-Date", stamp),
            ("X-Amz-Expires", str(expires)),
            ("X-Amz-SignedHeaders", "host"),
        ]
        sig = signature(self.secret, "GET", key_path, query, {"host": HOST}, ["host"], UNSIGNED, stamp, self.region)
        return BASE + uri_encode(key_path, True) + "?" + canonical_query(query + [("X-Amz-Signature", sig)])


def fetch(method, url, headers, body=b""):
    req = urllib.request.Request(url, data=body if method == "PUT" else None, method=method, headers=headers)
    for _ in range(100):
        try:
            with urllib.request.urlopen(req) as r:
                return r.status, r.read(), dict(r.headers)
        except urllib.error.HTTPError as e:
            return e.code, e.read(), dict(e.headers)
        except urllib.error.URLError:
            time.sleep(0.02)
    raise SystemExit("the store never answered")


def short(data):
    return f"{len(data)} bytes, sha256 {hashlib.sha256(data).hexdigest()[:16]}"


def list_all(who, bucket, prefix, delim, max_keys, label):
    token = None
    page = 1
    while True:
        q = [("list-type", "2"), ("prefix", prefix), ("max-keys", str(max_keys))]
        if delim:
            q.append(("delimiter", delim))
        if token:
            q.append(("continuation-token", token))
        status, body, _ = who.send("GET", f"/{bucket}", q)
        if status != 200:
            print(f"list {label} page {page} -> {describe(status, body)}")
            return
        text = body.decode()
        keys = [f"{k} ({s})" for k, s in zip(tag(text, "Key"), tag(text, "Size"))]
        prefixes = tag(text, "Prefix")[1:]
        print(f"list {label} page {page} -> {tag(text, 'KeyCount')[0]} entries, truncated {tag(text, 'IsTruncated')[0]}")
        for k in keys:
            print(f"  key {k}")
        for p in prefixes:
            print(f"  prefix {p}")
        nxt = tag(text, "NextContinuationToken")
        if not nxt:
            return
        token = nxt[0]
        page += 1


def main():
    # AWS's published get-vanilla example (its documentation's fake key).
    secret = "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY"
    creq = "GET\n/\n\nhost:example.amazonaws.com\nx-amz-date:20150830T123600Z\n\nhost;x-amz-date\n" + EMPTY_SHA256
    sts = "\n".join([ALGORITHM, "20150830T123600Z", "20150830/us-east-1/service/aws4_request", hashlib.sha256(creq.encode()).hexdigest()])
    k = hmac256(("AWS4" + secret).encode(), b"20150830")
    for part in (b"us-east-1", b"service", b"aws4_request"):
        k = hmac256(k, part)
    vanilla = hmac.new(k, sts.encode(), hashlib.sha256).hexdigest()
    print(f"sigv4 get-vanilla signature {vanilla}")

    server = http.server.ThreadingHTTPServer(ADDR, Handler)
    threading.Thread(target=server.serve_forever, daemon=True).start()

    writer = Signer("AKIDKARAWRITER01", KEYS["AKIDKARAWRITER01"][0])
    reader = Signer("AKIDKARAREADER01", KEYS["AKIDKARAREADER01"][0])

    print("== buckets")
    for name in ("kara-assets", "kara-assets", "Bad_Bucket", "kara-logs"):
        status, body, _ = writer.send("PUT", f"/{name}")
        print(f"create {name} -> {describe(status, body)}")
    status, body, _ = reader.send("GET", "/")
    print(f"buckets -> {status} {', '.join(tag(body.decode(), 'Name'))}")

    print("== put")
    blob = bytes(range(256))
    chain = hashlib.sha256(b"beach").digest() + hashlib.sha256(b"sand").digest()
    objects = [
        ("readme.txt", "Kara assets bucket.\nSigned with SigV4.\n".encode(), "text/plain"),
        ("logo.bin", blob, "application/octet-stream"),
        ("photos/2026/café menu.txt", "soup 4\nbread 2\n".encode(), "text/plain; charset=utf-8"),
        ("photos/2026/beach.raw", chain, "image/x-raw"),
        ("photos/2025/old.txt", b"from last year\n", "text/plain"),
        ("notes/a b+c.txt", b"plus, space\n", "text/plain"),
    ]
    for key, data, ctype in objects:
        status, body, headers = writer.send("PUT", f"/kara-assets/{key}", body=data, ctype=ctype)
        print(f"put {key} -> {describe(status, body)} etag {headers.get('ETag', '-')}")

    print("== get")
    for key, data, _ in objects:
        status, body, headers = reader.send("GET", f"/kara-assets/{key}")
        same = "same" if body == data else "DIFFERENT"
        print(f"get {key} -> {status} {short(body)}, {headers.get('Content-Type')}, {same}")

    print("== list")
    list_all(reader, "kara-assets", "", "/", 1000, "top")
    list_all(reader, "kara-assets", "photos/", "/", 1000, "photos/")
    list_all(reader, "kara-assets", "", "", 2, "all by 2")
    list_all(reader, "kara-assets", "", "/", 2, "top by 2")
    list_all(reader, "kara-nope", "", "", 10, "missing bucket")

    print("== refused")
    status, body, _ = reader.send("PUT", "/kara-assets/x.txt", body=b"no")
    print(f"reader put -> {describe(status, body)}")
    status, body, _ = Signer("AKIDKARAWRITER01", "guessed-secret").send("GET", "/kara-assets/readme.txt")
    print(f"wrong secret -> {describe(status, body)}")
    status, body, _ = Signer("AKIDKARANOBODY01", "x").send("GET", "/kara-assets/readme.txt")
    print(f"unknown key -> {describe(status, body)}")
    status, body, _ = Signer("AKIDKARAWRITER01", KEYS["AKIDKARAWRITER01"][0], "us-west-2").send("GET", "/kara-assets/readme.txt")
    print(f"wrong region -> {describe(status, body)}")
    status, body, _ = writer.send("GET", "/kara-assets/readme.txt", stamp="20261123T110000Z")
    print(f"an hour slow -> {describe(status, body)}")
    status, body, _ = writer.send("GET", "/kara-assets/readme.txt", stamp="20261123T121000Z")
    print(f"ten minutes fast -> {describe(status, body)}")
    status, body, _ = writer.send("PUT", "/kara-assets/y.txt", body=b"tampered", payload=hashlib.sha256(b"original").hexdigest())
    print(f"body swapped -> {describe(status, body)}")
    status, body, _ = writer.send("PUT", "/kara-assets/z.txt", body=b"streamed", payload=UNSIGNED)
    print(f"unsigned payload -> {describe(status, body)}")
    status, body, _ = writer.send("GET", "/kara-assets/readme.txt", path_sent="/kara-assets/logo.bin")
    print(f"path swapped -> {describe(status, body)}")
    status, body, _ = fetch("GET", BASE + "/kara-assets/readme.txt", {})
    print(f"not signed -> {describe(status, body)}")
    status, body, _ = fetch("GET", BASE + "/kara-assets/readme.txt", {"Authorization": "AWS4-HMAC-SHA1 Credential=x", "x-amz-date": NOW})
    print(f"wrong algorithm -> {describe(status, body)}")
    status, body, _ = fetch("GET", BASE + "/kara-assets/read%zzme.txt", {})
    print(f"bad escape -> {describe(status, body)}")

    print("== presigned")
    url = writer.presign("/kara-assets/photos/2026/café menu.txt", 300)
    status, body, _ = fetch("GET", url, {})
    print(f"presigned menu -> {status} {short(body)}")
    url = writer.presign("/kara-assets/readme.txt", 600, stamp="20261123T113000Z")
    status, body, _ = fetch("GET", url, {})
    print(f"presigned 30 minutes ago for 10 -> {describe(status, body)}")
    url = writer.presign("/kara-assets/readme.txt", 3600, stamp="20261123T113000Z")
    status, body, _ = fetch("GET", url, {})
    print(f"presigned 30 minutes ago for 60 -> {status} {short(body)}")
    url = writer.presign("/kara-assets/readme.txt", 300).replace("readme", "logo.b").replace(".txt", "in")
    status, body, _ = fetch("GET", url, {})
    print(f"presigned then edited -> {describe(status, body)}")
    url = writer.presign("/kara-assets/readme.txt", 999999)
    status, body, _ = fetch("GET", url, {})
    print(f"presigned for 12 days -> {describe(status, body)}")

    print("== delete")
    status, body, _ = writer.send("DELETE", "/kara-assets/logo.bin")
    print(f"delete logo.bin -> {describe(status, body)}")
    status, body, _ = writer.send("DELETE", "/kara-assets/logo.bin")
    print(f"delete logo.bin again -> {describe(status, body)}")
    status, body, _ = reader.send("GET", "/kara-assets/logo.bin")
    print(f"get logo.bin -> {describe(status, body)}")
    list_all(reader, "kara-assets", "", "/", 1000, "after delete")

    print("== stats")
    status, body, _ = fetch("GET", BASE + "/_stats", {})
    print(f"stats -> {status} {body.decode()}")
    server.shutdown()
    print("done")


if __name__ == "__main__":
    main()
