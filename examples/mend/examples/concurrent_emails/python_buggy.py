"""Same task in Python — sends welcomes concurrently, counts them.

Passes mypy and pyright. Runs "correctly" most of the time. Has a data
race on the shared counter: `self.sent += 1` is load-add-store, three
bytecodes, and a thread switch between the load and the store drops an
increment. No Python static checker can see it — the types are right,
the control flow is right; the bug is a property of shared mutable
state under concurrency, which is outside the type system's remit.
"""

import threading


class Mailer:
    def __init__(self) -> None:
        self.sent: int = 0

    def send_welcome(self, user_id: int) -> None:
        print("welcome user")
        self.sent += 1  # <-- data race: not atomic


def main() -> None:
    mailer = Mailer()
    threads = [
        threading.Thread(target=mailer.send_welcome, args=(uid,))
        for uid in (1, 2, 3)
    ]
    for t in threads:
        t.start()
    for t in threads:
        t.join()
    print(f"done: sent {mailer.sent}")


if __name__ == "__main__":
    main()
