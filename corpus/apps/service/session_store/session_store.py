"""Python mirror of source.orig.kara: session store over a logical clock."""

HASH_MOD = 1000000007
HASH_BASE = 131


class Config:
    def __init__(self, idle_timeout, absolute_timeout, max_per_user, sweep_interval):
        self.idle_timeout = idle_timeout
        self.absolute_timeout = absolute_timeout
        self.max_per_user = max_per_user
        self.sweep_interval = sweep_interval


class Session:
    def __init__(self, token, user, created, last_seen):
        self.token = token
        self.user = user
        self.created = created
        self.last_seen = last_seen

    def idle_deadline(self, cfg):
        return self.last_seen + cfg.idle_timeout

    def absolute_deadline(self, cfg):
        return self.created + cfg.absolute_timeout

    def expires_at(self, cfg):
        return min(self.idle_deadline(cfg), self.absolute_deadline(cfg))

    def expiry(self, cfg, now):
        if now >= self.absolute_deadline(cfg):
            return "absolute"
        if now >= self.idle_deadline(cfg):
            return "idle"
        return None


class SessionError(Exception):
    def __init__(self, kind, subject, why=None):
        self.kind = kind
        self.subject = subject
        self.why = why

    def describe(self):
        if self.kind == "unknown_user":
            return f"unknown user '{self.subject}'"
        if self.kind == "bad_password":
            return f"bad password for '{self.subject}'"
        if self.kind == "unknown_token":
            return f"unknown token {self.subject}"
        return f"token {self.subject} expired ({self.why})"


def hex8(n):
    return format(n & 0xFFFFFFFF, "08x")


def derive_token(user, t, serial):
    seed = f"{user}|{t}|{serial}"
    h = 7
    for c in seed:
        h = (h * HASH_BASE + ord(c)) % HASH_MOD
    h = (h * HASH_BASE + serial * 977) % HASH_MOD
    return f"s-{hex8(h)}"


class Store:
    def __init__(self, cfg):
        self.cfg = cfg
        self.credentials = {}
        self.sessions = {}
        self.serial = 0
        self.next_sweep = cfg.sweep_interval

    def add_user(self, user, password):
        self.credentials[user] = password

    def sessions_of(self, user):
        return [tok for tok in sorted(self.sessions) if self.sessions[tok].user == user]

    def oldest_of(self, user):
        best = None
        best_created = 0
        for tok in sorted(self.sessions):
            s = self.sessions[tok]
            if s.user == user:
                if best is None or s.created < best_created:
                    best = tok
                    best_created = s.created
        return best

    def login(self, user, password, now):
        if user not in self.credentials:
            raise SessionError("unknown_user", user)
        if self.credentials[user] != password:
            raise SessionError("bad_password", user)
        evicted = None
        if len(self.sessions_of(user)) >= self.cfg.max_per_user:
            old = self.oldest_of(user)
            if old is not None:
                del self.sessions[old]
                evicted = old
        self.serial += 1
        token = derive_token(user, now, self.serial)
        self.sessions[token] = Session(token, user, now, now)
        return token, evicted

    def live_session(self, token, now):
        session = self.sessions.pop(token, None)
        if session is None:
            raise SessionError("unknown_token", token)
        why = session.expiry(self.cfg, now)
        if why is not None:
            raise SessionError("expired", token, why)
        return session

    def refresh(self, token, now):
        session = self.live_session(token, now)
        session.last_seen = now
        deadline = session.expires_at(self.cfg)
        self.sessions[token] = session
        return deadline

    def check(self, token, now):
        session = self.live_session(token, now)
        self.sessions[token] = session
        return session.user

    def logout(self, token, now):
        return self.live_session(token, now).user

    def sweep(self, now):
        expired = [tok for tok in sorted(self.sessions)
                   if self.sessions[tok].expiry(self.cfg, now) is not None]
        for tok in expired:
            del self.sessions[tok]
        return expired

    def run_due_sweeps(self, now):
        while self.next_sweep <= now:
            at = self.next_sweep
            removed = self.sweep(at)
            if not removed:
                print(f"  [sweep t={at}] nothing expired")
            else:
                print(f"  [sweep t={at}] removed {len(removed)}: {', '.join(removed)}")
            self.next_sweep = at + self.cfg.sweep_interval

    def dump(self, now):
        print(f"store at t={now}: {len(self.sessions)} session(s)")
        for tok in sorted(self.sessions):
            s = self.sessions[tok]
            why = s.expiry(self.cfg, now)
            state = f"stale:{why}" if why is not None else "live"
            print(f"  {tok} user={s.user} created={s.created} last_seen={s.last_seen} "
                  f"expires={s.expires_at(self.cfg)} {state}")


def resolve(ref, issued):
    kind, val = ref
    if kind == "issued":
        return issued[val] if val < len(issued) else f"s-unissued{val}"
    return val


def requests():
    def login(t, u, p):
        return (t, "login", (u, p))

    def iss(i):
        return ("issued", i)

    return [
        login(1, "alice", "wonderland"),
        login(3, "bob", "builder"),
        login(4, "carol", "guess"),
        login(5, "mallory", "letmein"),
        (10, "refresh", iss(0)),
        login(12, "alice", "wonderland"),
        (15, "check", iss(1)),
        login(20, "alice", "wonderland"),
        (22, "refresh", iss(0)),
        (25, "refresh", iss(2)),
        (38, "check", iss(2)),
        login(41, "carol", "sunshine"),
        (45, "logout", iss(4)),
        (46, "logout", iss(4)),
        (50, "refresh", ("literal", "s-deadbeef")),
        login(52, "bob", "builder"),
        (60, "refresh", iss(3)),
        (75, "refresh", iss(5)),
        (90, "refresh", iss(3)),
        (100, "refresh", iss(5)),
        login(104, "alice", "wonderland"),
        login(106, "alice", "wonderland"),
        login(108, "alice", "wonderland"),
        (110, "check", iss(6)),
        (118, "check", iss(8)),
        (125, "refresh", iss(5)),
        (130, "refresh", iss(8)),
        (140, "refresh", iss(7)),
        (145, "check", iss(5)),
        (150, "logout", iss(8)),
        login(152, "carol", "sunshine"),
        login(155, "bob", "builder"),
        (158, "check", iss(10)),
        login(184, "alice", "wonderland"),
    ]


def main():
    cfg = Config(idle_timeout=30, absolute_timeout=90, max_per_user=2, sweep_interval=40)
    store = Store(cfg)
    store.add_user("alice", "wonderland")
    store.add_user("bob", "builder")
    store.add_user("carol", "sunshine")

    issued = []
    ok_count = 0
    err_count = 0
    last_t = 0
    for t, op, arg in requests():
        store.run_due_sweeps(t)
        last_t = t
        try:
            if op == "login":
                user, password = arg
                token, evicted = store.login(user, password, t)
                issued.append(token)
                if evicted is not None:
                    msg = f"login {user} -> {token} (evicted {evicted})"
                else:
                    msg = f"login {user} -> {token}"
            elif op == "logout":
                tok = resolve(arg, issued)
                msg = f"logout {tok} ({store.logout(tok, t)})"
            elif op == "refresh":
                tok = resolve(arg, issued)
                msg = f"refresh {tok} -> expires {store.refresh(tok, t)}"
            else:
                tok = resolve(arg, issued)
                msg = f"check {tok} -> {store.check(tok, t)}"
            ok_count += 1
            print(f"t={t} ok   {msg}")
        except SessionError as e:
            err_count += 1
            print(f"t={t} FAIL {e.describe()}")
    store.dump(last_t)
    print(f"requests: {ok_count} ok, {err_count} failed, {len(issued)} tokens issued")


if __name__ == "__main__":
    main()
