"""Python mirror of solution.kara: the oracle for expected.txt.

Three welcome lines built concurrently, a lock-guarded sent-count, then the
lines in user order, the count, and "done".
"""
import threading
from concurrent.futures import ThreadPoolExecutor


class Counter:
    def __init__(self):
        self.count = 0
        self.lock = threading.Lock()


def send_welcome(user_id, counter):
    with counter.lock:
        counter.count += 1
    return f"welcome user {user_id}"


def main():
    counter = Counter()
    with ThreadPoolExecutor(max_workers=3) as pool:
        futures = [pool.submit(send_welcome, u, counter) for u in (1, 2, 3)]
        lines = [f.result() for f in futures]
    for line in lines:
        print(line)
    print(f"sent {counter.count}")
    print("done")


if __name__ == "__main__":
    main()
