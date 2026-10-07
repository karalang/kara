"""Python mirror of source.orig.kara: in-process HTTP router."""

METHODS = ["GET", "POST", "PUT", "DELETE", "PATCH"]
AUTH_TOKEN = "Bearer s3cret"


class ApiError(Exception):
    def __init__(self, status, msg):
        self.status = status
        self.msg = msg


def bad_request(msg):
    return ApiError(400, msg)


def not_found(msg):
    return ApiError(404, msg)


def conflict(msg):
    return ApiError(409, msg)


def parse_method(raw):
    for m in METHODS:
        if m == raw.upper():
            return m, None
    return None, f"method {raw} not supported"


class Request:
    def __init__(self, method, path, headers, body):
        self.method = method
        self.path = path
        self.headers = headers
        self.body = body

    def header(self, name):
        wanted = name.lower()
        for k, v in self.headers:
            if k.lower() == wanted:
                return v
        return None


class User:
    def __init__(self, uid, name, email):
        self.id = uid
        self.name = name
        self.email = email


class UserStore:
    def __init__(self, users, next_id):
        self.users = users
        self.next_id = next_id


def split_path(path):
    return [p for p in path.split("/") if p != ""]


def compile_pattern(pattern):
    segs = []
    for part in split_path(pattern):
        if part.startswith(":"):
            segs.append(("param", part[1:]))
        elif part.startswith("*"):
            segs.append(("tail", part[1:]))
        else:
            segs.append(("static", part))
    return segs


def match_pattern(pattern, parts):
    params = {}
    i = 0
    for kind, text in pattern:
        if kind == "static":
            if i >= len(parts) or parts[i] != text:
                return None
            i += 1
        elif kind == "param":
            if i >= len(parts):
                return None
            params[text] = parts[i]
            i += 1
        else:
            rest = []
            while i < len(parts):
                rest.append(parts[i])
                i += 1
            params[text] = "/".join(rest)
    return params if i == len(parts) else None


class Router:
    def __init__(self):
        self.routes = []

    def add(self, method, pattern, handler):
        self.routes.append((method, compile_pattern(pattern), handler))

    def lookup(self, method, path):
        parts = split_path(path)
        allowed = []
        for index, (rmethod, pattern, _handler) in enumerate(self.routes):
            params = match_pattern(pattern, parts)
            if params is not None:
                if rmethod == method:
                    return ("matched", index, params)
                if rmethod not in allowed:
                    allowed.append(rmethod)
        if not allowed:
            return ("not_found", None, None)
        return ("not_allowed", allowed, None)


def parse_form(body):
    fields = {}
    if body.strip() == "":
        return fields
    for pair in body.split("&"):
        kv = pair.split("=")
        if len(kv) != 2 or kv[0].strip() == "":
            raise bad_request(f"malformed field '{pair}'")
        fields[kv[0].strip()] = kv[1].strip()
    return fields


def parse_i64(raw):
    try:
        if raw.strip() != raw or raw == "":
            return None
        return int(raw, 10)
    except ValueError:
        return None


def param_id(ctx, key):
    raw = ctx["params"].get(key, "")
    v = parse_i64(raw)
    if v is None:
        raise bad_request(f"invalid id '{raw}'")
    return v


def email_owner(store, email):
    for u in store.users.values():
        if u.email == email:
            return u.id
    return None


def health(ctx, store):
    return 200, f"ok ({len(store.users)} users)"


def list_users(ctx, store):
    ids = sorted(store.users.keys())
    entries = [f"{store.users[i].id}:{store.users[i].name}" for i in ids]
    if not entries:
        entries.append("(none)")
    return 200, "users: " + ",".join(entries)


def get_user(ctx, store):
    uid = param_id(ctx, "id")
    u = store.users.get(uid)
    if u is None:
        raise not_found(f"user {uid} not found")
    return 200, f"user {u.id} {u.name} <{u.email}>"


def create_user(ctx, store):
    form = parse_form(ctx["body"])
    name = form.pop("name", None)
    if name is None:
        raise bad_request("missing field 'name'")
    email = form.pop("email", None)
    if email is None:
        raise bad_request("missing field 'email'")
    if "@" not in email:
        raise bad_request(f"invalid email '{email}'")
    owner = email_owner(store, email)
    if owner is not None:
        raise conflict(f"email {email} already used by user {owner}")
    uid = store.next_id
    store.next_id += 1
    body = f"created user {uid} {name}"
    store.users[uid] = User(uid, name, email)
    return 201, body


def update_user(ctx, store):
    uid = param_id(ctx, "id")
    form = parse_form(ctx["body"])
    user = store.users.pop(uid, None)
    if user is None:
        raise not_found(f"user {uid} not found")
    if "name" in form:
        user.name = form.pop("name")
    if "email" in form:
        user.email = form.pop("email")
    body = f"updated user {user.id} {user.name} <{user.email}>"
    store.users[uid] = user
    return 200, body


def delete_user(ctx, store):
    uid = param_id(ctx, "id")
    if store.users.pop(uid, None) is None:
        raise not_found(f"user {uid} not found")
    return 204, ""


def serve_file(ctx, store):
    path = ctx["params"].get("path", "")
    if path == "":
        return 200, "file index"
    return 200, f"file {path}"


REASONS = {
    200: "OK", 201: "Created", 204: "No Content", 400: "Bad Request",
    401: "Unauthorized", 403: "Forbidden", 404: "Not Found",
    405: "Method Not Allowed", 409: "Conflict", 501: "Not Implemented",
}


def reason(status):
    return REASONS.get(status, "Unknown")


def pad4(n):
    s = str(n)
    while len(s) < 4:
        s = "0" + s
    return s


class App:
    def __init__(self, router, store):
        self.router = router
        self.store = store
        self.chain = ["request_id", "logger", "auth"]
        self.log = []
        self.request_id = ""
        self.id_counter = 0

    def before(self, mw, req):
        if mw == "request_id":
            given = req.header("x-request-id")
            if given is not None:
                self.request_id = given
            else:
                self.id_counter += 1
                self.request_id = "req-" + pad4(self.id_counter)
            return None
        if mw == "logger":
            self.log.append(f"{self.request_id} start {req.method} {req.path}")
            return None
        if req.method.upper() == "GET" or not req.path.startswith("/users"):
            return None
        token = req.header("authorization")
        if token is None:
            token = ""
        if token == AUTH_TOKEN:
            return None
        if token == "":
            status, why = 401, "missing credentials"
        else:
            status, why = 403, "invalid token"
        self.log.append(f"{self.request_id} auth denied: {why}")
        return (status, why)

    def after(self, mw, reply):
        if mw == "logger":
            self.log.append(f"{self.request_id} end {reply[0]}")

    def dispatch(self, req):
        method, err = parse_method(req.method)
        if method is None:
            return (501, err)
        kind, a, params = self.router.lookup(method, req.path)
        if kind == "matched":
            handler = self.router.routes[a][2]
            ctx = {"params": params, "body": req.body}
            try:
                return handler(ctx, self.store)
            except ApiError as e:
                return (e.status, e.msg)
        if kind == "not_allowed":
            return (405, "allowed: " + ", ".join(a))
        return (404, f"no route for {req.path}")

    def handle(self, req):
        ran = 0
        early = None
        while ran < len(self.chain):
            mw = self.chain[ran]
            ran += 1
            r = self.before(mw, req)
            if r is not None:
                early = r
                break
        reply = early if early is not None else self.dispatch(req)
        while ran > 0:
            ran -= 1
            self.after(self.chain[ran], reply)
        status, body = reply
        line = f"[{self.request_id}] {req.method} {req.path} -> {status} {reason(status)}"
        if body != "":
            line += f": {body}"
        return line


def build_app():
    router = Router()
    router.add("GET", "/health", health)
    router.add("GET", "/users", list_users)
    router.add("POST", "/users", create_user)
    router.add("GET", "/users/:id", get_user)
    router.add("PUT", "/users/:id", update_user)
    router.add("DELETE", "/users/:id", delete_user)
    router.add("GET", "/files/*path", serve_file)
    users = {
        1: User(1, "alice", "alice@example.com"),
        2: User(2, "bob", "bob@example.com"),
    }
    return App(router, UserStore(users, 3))


def main():
    app = build_app()
    ok_auth = ("Authorization", AUTH_TOKEN)
    R = Request
    requests = [
        R("GET", "/health", [], ""),
        R("GET", "/users", [], ""),
        R("GET", "/users/1", [], ""),
        R("GET", "/users/42", [], ""),
        R("GET", "/users/abc", [], ""),
        R("POST", "/users", [], "name=carol&email=carol@example.com"),
        R("POST", "/users", [("authorization", "Bearer wrong")], "name=carol&email=carol@example.com"),
        R("POST", "/users", [ok_auth], "name=carol&email=carol@example.com"),
        R("POST", "/users", [ok_auth], "name=dave&email=alice@example.com"),
        R("POST", "/users", [ok_auth], "name=erin"),
        R("POST", "/users", [ok_auth], "name&email"),
        R("PUT", "/users/2", [ok_auth, ("Content-Type", "form")], "name=robert"),
        R("DELETE", "/users/1", [ok_auth, ("X-Request-Id", "trace-abc")], ""),
        R("GET", "/users", [], ""),
        R("DELETE", "/health", [], ""),
        R("PATCH", "/users/2", [ok_auth], "name=bobby"),
        R("GET", "/nope", [], ""),
        R("GET", "/files/docs/guide/intro.md", [], ""),
        R("GET", "/files", [], ""),
        R("options", "/health", [], ""),
    ]
    for r in requests:
        print(app.handle(r))
    print("--- log ---")
    for entry in app.log:
        print(entry)


if __name__ == "__main__":
    main()
