"""Python mirror of source.orig.kara: layered configuration resolver."""


class Kind:
    def __init__(self, tag, lo=0, hi=0, options=None):
        self.tag = tag
        self.lo = lo
        self.hi = hi
        self.options = options or []


def k_int(lo, hi):
    return Kind("int", lo, hi)


K_FLAG = Kind("flag")
K_TEXT = Kind("text")
K_LIST = Kind("list")


def k_choice(options):
    return Kind("choice", options=options)


class Value:
    def __init__(self, tag, data):
        self.tag = tag
        self.data = data

    def render(self):
        if self.tag == "int":
            return str(self.data)
        if self.tag == "bool":
            return "true" if self.data else "false"
        if self.tag == "str":
            return '"' + self.data + '"'
        return "[" + ", ".join(self.data) + "]"


class ConfigError(Exception):
    def __init__(self, message):
        self.message = message

    def __str__(self):
        return self.message


def layer_label(layer):
    kind, path = layer
    if kind == "file":
        return "file:" + path
    return kind


def parse_bool(text):
    lower = text.lower()
    if lower in ("true", "yes", "on", "1"):
        return True
    if lower in ("false", "no", "off", "0"):
        return False
    return None


def parse_int(text):
    # i64.parse: optional sign followed by ASCII digits
    body = text[1:] if text[:1] in ("+", "-") else text
    if not body or not all("0" <= c <= "9" for c in body):
        return None
    return int(text)


def coerce(key, raw, kind, layer):
    text = raw.strip()
    if kind.tag == "int":
        n = parse_int(text)
        if n is None:
            raise ConfigError(f"{layer}: {key}: expected an integer, got '{text}'")
        if n < kind.lo or n > kind.hi:
            raise ConfigError(f"{layer}: {key}: {n} is outside [{kind.lo}, {kind.hi}]")
        return Value("int", n)
    if kind.tag == "flag":
        b = parse_bool(text)
        if b is None:
            raise ConfigError(f"{layer}: {key}: expected a boolean, got '{text}'")
        return Value("bool", b)
    if kind.tag == "text":
        return Value("str", text)
    if kind.tag == "choice":
        if text in kind.options:
            return Value("str", text)
        options = "|".join(kind.options)
        raise ConfigError(f"{layer}: {key}: '{text}' is not one of {options}")
    items = []
    for part in text.split(","):
        item = part.strip()
        if item:
            items.append(item)
    return Value("list", items)


class Config:
    def __init__(self, schema):
        self.schema = schema
        self.entries = {}
        self.errors = []

    def apply(self, key, raw, layer):
        kind = self.schema.get(key)
        if kind is None:
            self.errors.append(ConfigError(f"{layer_label(layer)}: unknown key '{key}'"))
            return
        try:
            value = coerce(key, raw, kind, layer_label(layer))
        except ConfigError as e:
            self.errors.append(e)
            return
        self.entries[key] = (value, layer)

    def int(self, key):
        entry = self.entries.get(key)
        if entry is not None and entry[0].tag == "int":
            return entry[0].data
        return None

    def validate(self, required):
        for key in required:
            if key not in self.entries:
                self.errors.append(ConfigError(f"required key '{key}' is not set"))
        workers = self.int("server.workers")
        workers = 0 if workers is None else workers
        pool = self.int("db.pool")
        pool = 0 if pool is None else pool
        if workers > pool:
            self.errors.append(ConfigError(
                f"constraint: server.workers ({workers}) must not exceed db.pool ({pool})"))


def schema():
    return {
        "server.host": K_TEXT,
        "server.port": k_int(1, 65535),
        "server.workers": k_int(1, 256),
        "log.level": k_choice(["debug", "info", "warn", "error"]),
        "log.json": K_FLAG,
        "log.file_path": K_TEXT,
        "cache.enabled": K_FLAG,
        "cache.ttl": k_int(0, 86400),
        "features.flags": K_LIST,
        "db.url": K_TEXT,
        "db.pool": k_int(1, 64),
        "auth.token": K_TEXT,
        "metrics.interval": k_int(1, 3600),
    }


def defaults():
    return [
        ("server.host", "localhost"),
        ("server.port", "8080"),
        ("server.workers", "4"),
        ("log.level", "info"),
        ("log.json", "false"),
        ("log.file_path", "/var/log/app.log"),
        ("cache.enabled", "true"),
        ("cache.ttl", "300"),
        ("features.flags", "search"),
        ("db.pool", "8"),
        ("metrics.interval", "60"),
    ]


def load_file(cfg, path, text):
    section = ""
    line_no = 0
    for raw_line in text.split("\n"):
        line_no += 1
        line = raw_line.strip()
        if not line or line.startswith("#"):
            continue
        if line.startswith("[") and line.endswith("]"):
            section = line[1:len(line) - 1].strip()
            continue
        pos = line.find("=")
        if pos < 0:
            cfg.errors.append(ConfigError(f"{path}:{line_no}: cannot parse '{line}'"))
            continue
        name = line[:pos].strip()
        value = line[pos + 1:].strip()
        key = name if not section else f"{section}.{name}"
        cfg.apply(key, value, ("file", path))


def load_env(cfg, env_vars):
    prefix = "APP_"
    index = 0
    for var in env_vars:
        index += 1
        if not var.startswith(prefix):
            continue
        pos = var.find("=")
        if pos < 0:
            cfg.errors.append(ConfigError(f"env:{index}: cannot parse '{var}'"))
            continue
        name = var[len(prefix):pos]
        key = name.lower().replace("__", ".")
        cfg.apply(key, var[pos + 1:], ("env", ""))


def load_cli(cfg, args):
    index = 0
    for arg in args:
        index += 1
        if not arg.startswith("--"):
            cfg.errors.append(ConfigError(f"cli:{index}: cannot parse '{arg}'"))
            continue
        body = arg[2:]
        pos = body.find("=")
        if pos >= 0:
            cfg.apply(body[:pos], body[pos + 1:], ("cli", ""))
        elif body.startswith("no-"):
            cfg.apply(body[3:], "false", ("cli", ""))
        else:
            cfg.apply(body, "true", ("cli", ""))


def pad(text, width):
    return text + " " * max(0, width - len(text.encode("utf-8")))


def report(cfg):
    key_width = 0
    for key in cfg.entries:
        key_width = max(key_width, len(key))
    print(f"resolved {len(cfg.entries)} keys:")
    wins = {}
    for key in sorted(cfg.entries):
        value, layer = cfg.entries[key]
        label = layer_label(layer)
        print(f"  {pad(key, key_width)} = {pad(value.render(), 34)} ({label})")
        wins[label] = wins.get(label, 0) + 1
    print("winning layers:")
    for label in sorted(wins):
        print(f"  {label}: {wins[label]}")
    print(f"errors ({len(cfg.errors)}):")
    for e in cfg.errors:
        print(f"  {e}")


def main():
    cfg = Config(schema())
    for key, raw in defaults():
        cfg.apply(key, raw, ("default", ""))

    base = ("# application config\n[server]\nhost = api.example.org\nport = 8443\n\n"
            "[log]\nlevel = info\njson = yes\n\n[db]\npool = 16\nretries = 3\n")
    local = ("[server]\nhost = staging.example.org\nthis line is broken\n[cache]\nttl = 600\n"
             "[log]\nlevel = verbose\nfile_path = /tmp/app-staging.log\n")

    with open("app.conf", "w") as f:
        f.write(base)
    with open("app.local.conf", "w") as f:
        f.write(local)
    with open("app.conf") as f:
        read_base = f.read()
    with open("app.local.conf") as f:
        read_local = f.read()

    load_file(cfg, "app.conf", read_base)
    load_file(cfg, "app.local.conf", read_local)

    env_vars = [
        "HOME=/home/app",
        "APP_SERVER__PORT=9090",
        "APP_LOG__LEVEL=warn",
        "APP_FEATURES__FLAGS=search, beta ,,metrics",
        "APP_CACHE__TTL=abc",
        "APP_BROKEN",
        "APP_DB__URL=postgres://db.internal:5432/app?sslmode=require",
        "APP_LOG__JSON=maybe",
        "PATH=/usr/bin",
    ]
    load_env(cfg, env_vars)

    args = [
        "--server.workers=32",
        "--log.json",
        "--no-cache.enabled",
        "--server.port=70000",
        "--metrics.interval=15s",
        "--colour=always",
        "positional",
        "--features.flags=search,export",
    ]
    load_cli(cfg, args)

    cfg.validate(["db.url", "auth.token"])
    report(cfg)


main()
