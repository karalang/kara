# design_studies/event_stream/event_stream.py
#
# Read JSON events line-by-line from stdin and print a one-line
# summary for each. Unbounded push-model source — runs until EOF.
#
# Input shape (one per line):
#   {"event": "login", "user": "alice"}

import json
import sys


def main() -> None:
    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        try:
            event = json.loads(line)
            print(f"[{event['event']}] {event['user']}")
        except (json.JSONDecodeError, KeyError):
            print(f"bad event: {line}", file=sys.stderr)


if __name__ == "__main__":
    main()
