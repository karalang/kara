#!/usr/bin/env python3
"""Python mirror of source.orig.kara: job queue with workers, retries,
exponential backoff in logical ticks, and a dead-letter queue."""

import time

KIND_NAMES = {"Email": "email", "Report": "report", "Webhook": "webhook", "Resize": "resize"}
KIND_INDEX = {"Email": 0, "Report": 1, "Webhook": 2, "Resize": 3}


class Transient(Exception):
    pass


class Permanent(Exception):
    pass


class Job:
    def __init__(self, id, kind, payload, priority):
        self.id = id
        self.kind = kind
        self.payload = payload
        self.priority = priority
        self.attempts = 0
        self.ready_at = 0

    def label(self):
        return f"#{self.id} {KIND_NAMES[self.kind]}({self.payload})"


class Config:
    def __init__(self, workers, max_attempts, base_backoff, max_ticks):
        self.workers = workers
        self.max_attempts = max_attempts
        self.base_backoff = base_backoff
        self.max_ticks = max_ticks


class Stats:
    def __init__(self):
        self.dispatched = 0
        self.succeeded = 0
        self.retried = 0
        self.dead = 0
        self.idle_ticks = 0


def backoff(base, attempt):
    delay = base
    n = 1
    while n < attempt:
        delay *= 2
        n += 1
    return delay


def validate(job):
    if job.payload == "":
        raise Permanent("empty payload")
    if " " in job.payload:
        raise Permanent("payload has whitespace")


def call_remote(job, attempt):
    """Returns ("ok", output) or ("transient"|"permanent", msg)."""
    time.sleep(0.002)
    try:
        validate(job)
    except Permanent as e:
        return ("permanent", str(e))
    if job.kind == "Email":
        return ("ok", f"sent to {job.payload}")
    if job.kind == "Report":
        if attempt < 3:
            return ("transient", "db busy")
        return ("ok", f"report {job.payload} rows={len(job.payload.encode()) * 7}")
    if job.kind == "Webhook":
        if job.payload.startswith("down"):
            return ("transient", "http 503")
        if attempt < 2:
            return ("transient", "timeout")
        return ("ok", f"delivered to {job.payload}")
    if job.kind == "Resize":
        if attempt < 2:
            return ("transient", "worker oom")
        return ("ok", f"resized {job.payload}")
    raise AssertionError("unknown kind")


class Queue:
    def __init__(self):
        self.pending = []
        self.dead_letter = []
        self.completed = []
        self.events = []
        self.stats = Stats()

    def submit(self, job):
        self.events.append(f"submit {job.label()} prio={job.priority}")
        self.pending.append(job)

    def log(self, tick, msg):
        self.events.append(f"[t{tick}] {msg}")

    def take_next(self, tick):
        best = -1
        for i, cand in enumerate(self.pending):
            if cand.ready_at <= tick:
                if best < 0:
                    best = i
                else:
                    cur = self.pending[best]
                    if cand.priority > cur.priority or (
                        cand.priority == cur.priority and cand.id < cur.id
                    ):
                        best = i
        if best < 0:
            return None
        return self.pending.pop(best)

    def settle(self, tick, cfg, outcome):
        w, job, (status, msg) = outcome
        if status == "ok":
            self.stats.succeeded += 1
            self.completed.append(job.id)
            self.log(tick, f"w{w} {job.label()} attempt {job.attempts} ok: {msg}")
        elif status == "permanent":
            self.stats.dead += 1
            self.log(tick, f"w{w} {job.label()} attempt {job.attempts} failed permanently: {msg} -> dead letter")
            self.dead_letter.append(job)
        else:
            if job.attempts >= cfg.max_attempts:
                self.stats.dead += 1
                self.log(tick, f"w{w} {job.label()} attempt {job.attempts} failed: {msg}; attempts exhausted -> dead letter")
                self.dead_letter.append(job)
            else:
                delay = backoff(cfg.base_backoff, job.attempts)
                job.ready_at = tick + delay
                self.stats.retried += 1
                self.log(tick, f"w{w} {job.label()} attempt {job.attempts} failed: {msg}; retry in {delay} at t{job.ready_at}")
                self.pending.append(job)


def run_worker(worker, slot):
    if slot is None:
        return None
    slot.attempts += 1
    result = call_remote(slot, slot.attempts)
    return (worker, slot, result)


def seed_jobs():
    return [
        Job(1, "Email", "alice@example.com", 5),
        Job(2, "Report", "q3-sales", 3),
        Job(3, "Webhook", "hooks.acme.io", 7),
        Job(4, "Resize", "cat.png", 1),
        Job(5, "Webhook", "down.legacy.net", 4),
        Job(6, "Email", "", 6),
        Job(7, "Report", "inventory", 3),
        Job(8, "Email", "bob@example.com", 2),
        Job(9, "Resize", "big photo.jpg", 8),
        Job(10, "Webhook", "pay.stripe.test", 9),
    ]


def main():
    cfg = Config(3, 4, 1, 50)
    queue = Queue()
    for job in seed_jobs():
        queue.submit(job)

    tick = 0
    while queue.pending and tick < cfg.max_ticks:
        s1 = queue.take_next(tick)
        s2 = queue.take_next(tick)
        s3 = queue.take_next(tick)

        o1 = run_worker(1, s1)
        o2 = run_worker(2, s2)
        o3 = run_worker(3, s3)

        ran = 0
        for slot in (o1, o2, o3):
            if slot is not None:
                ran += 1
                queue.stats.dispatched += 1
                queue.settle(tick, cfg, slot)
        if ran == 0:
            queue.stats.idle_ticks += 1
            queue.log(tick, f"idle ({len(queue.pending)} waiting)")
        tick += 1

    print("== event log ==")
    for line in queue.events:
        print(line)

    print("== dead letter queue ==")
    dead_ids = sorted(j.id for j in queue.dead_letter)
    if not dead_ids:
        print("(empty)")
    for id in dead_ids:
        for job in queue.dead_letter:
            if job.id == id:
                print(f"{job.label()} attempts={job.attempts}")

    print("== completion order ==")
    print(" ".join(f"#{i}" for i in queue.completed))

    per_kind = [0, 0, 0, 0]
    for job in seed_jobs():
        if job.id in queue.completed:
            per_kind[KIND_INDEX[job.kind]] += 1
    attempts_total = sum(j.attempts for j in queue.dead_letter)

    print("== stats ==")
    print(f"ticks: {tick}")
    print(f"workers: {cfg.workers}")
    print(f"dispatched: {queue.stats.dispatched}")
    print(f"succeeded: {queue.stats.succeeded}")
    print(f"retried: {queue.stats.retried}")
    print(f"dead: {queue.stats.dead}")
    print(f"dead-letter attempts: {attempts_total}")
    print(f"idle ticks: {queue.stats.idle_ticks}")
    print(f"ok by kind: email={per_kind[0]} report={per_kind[1]} webhook={per_kind[2]} resize={per_kind[3]}")
    print(f"still pending: {len(queue.pending)}")


if __name__ == "__main__":
    main()
