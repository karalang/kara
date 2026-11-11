# The Python mirror of source.kara (B-2026-11-11-34): expected.out is its output.
# threading.Semaphore waits in acquire and is uncapped in release, as the Kara one is.
import threading, time

class FooBar:
    def __init__(self, n):
        self.n = n
        self.foo_sem = threading.Semaphore(1)
        self.bar_sem = threading.Semaphore(0)
    def foo(self, print_foo):
        for _ in range(self.n):
            self.foo_sem.acquire(timeout=60)
            print_foo()
            self.bar_sem.release()
    def bar(self, print_bar):
        for _ in range(self.n):
            self.bar_sem.acquire(timeout=60)
            print_bar()
            self.foo_sem.release()

def run(n, bar_first):
    log = []
    fb = FooBar(n)
    a = threading.Thread(target=fb.foo, args=(lambda: log.append("foo"),))
    b = threading.Thread(target=fb.bar, args=(lambda: log.append("bar"),))
    ts = [b, a] if bar_first else [a, b]
    for t in ts: t.start()
    for t in ts: t.join()
    return "".join(log)

seed = [1115]
def next_random():
    seed[0] = (seed[0] * 1103515245 + 12345) % 2147483648
    return seed[0] // 65536

print(f"n = 1 -> {run(1, False)}")
print(f"n = 2 -> {run(2, False)}")
print(f"n = 3, bar started first -> {run(3, True)}")
alternating = 0
for _ in range(200):
    n = next_random() % 12 + 1
    bar_first = next_random() % 2 == 1
    if run(n, bar_first) == "foobar" * n:
        alternating += 1
print(f"200 runs of up to 12 rounds: {alternating} alternating")

gate = threading.Semaphore(0)
got = []
def waiter():
    got.append("released" if gate.acquire(timeout=60) else "timed out")
def releaser():
    time.sleep(0.02)
    gate.release()
ts = [threading.Thread(target=waiter), threading.Thread(target=releaser)]
for t in ts: t.start()
for t in ts: t.join()
print(f"a waiter: {''.join(got)}")

none = threading.Semaphore(0)
late = []
t = threading.Thread(target=lambda: late.append("released" if none.acquire(timeout=0.03) else "timed out"))
t.start(); t.join()
print(f"no releaser: {''.join(late)}")

s = threading.Semaphore(1)
s.release()
s.release()
taken = sum(1 for _ in range(4) if s.acquire(blocking=False))
print(f"one permit and two releases: {taken} acquired")
