#!/usr/bin/env python3
"""
micro_spec_fake_curl.py

Fake `curl` used ONLY by scripts/tests/test_run_micro_spec.sh (EN.ticket.
micro-spec-fixture-for-engine-seam-comparison task 5). Never touches a
network. Understands only the two calls scripts/run_micro_spec.sh makes:

  POST .../events/     -> prints {"event_id": "<distinct id>"} to stdout
                           and, as a side effect standing in for what the
                           real engine would do, writes/overwrites
                           planning/<spec>/sdlc/sdlc-flow-state.json with a
                           payload that embeds that SAME event_id. This is
                           what makes the deferred-harvest positive control
                           observable in the test: harvested FILE NAMES are
                           always distinct (the runner names each file
                           after the event_id from its own trigger
                           response), but when harvesting is deferred every
                           harvested file's CONTENT ends up holding the
                           LAST run's event_id, because each POST here
                           clobbers the same shared state path before an
                           earlier run's copy was ever taken.

  GET  .../events/<id> -> prints {"status": "succeeded"} immediately — no
                           polling loop needed for this test.

Distinct event ids come from an atomic counter file under
$MICRO_SPEC_CURL_STATE_DIR (set by the test harness), so ids stay distinct
across calls even inside the same wall-clock second.
"""
import json
import os
import sys


def next_event_id() -> str:
    state_dir = os.environ["MICRO_SPEC_CURL_STATE_DIR"]
    counter_path = os.path.join(state_dir, "event_counter")
    n = 0
    if os.path.exists(counter_path):
        with open(counter_path) as f:
            raw = f.read().strip()
            n = int(raw) if raw else 0
    n += 1
    with open(counter_path, "w") as f:
        f.write(str(n))
    return f"fake-event-{n}"


def main() -> int:
    argv = sys.argv[1:]
    method = "GET"
    data = None
    i = 0
    while i < len(argv):
        arg = argv[i]
        if arg == "-X":
            method = argv[i + 1]
            i += 2
            continue
        if arg == "-d":
            data = argv[i + 1]
            i += 2
            continue
        if arg == "-H":
            i += 2
            continue
        if arg in ("-s", "-f", "-sf", "-fs"):
            i += 1
            continue
        # Anything else (the URL) is consumed but not needed — the shim
        # never dials out, so it does not need to parse or route on it.
        i += 1

    if method == "POST":
        payload = json.loads(data) if data else {}
        spec = payload.get("data", {}).get("spec_slug", "unknown-spec")
        event_id = next_event_id()

        state_path = os.path.join("planning", spec, "sdlc", "sdlc-flow-state.json")
        os.makedirs(os.path.dirname(state_path), exist_ok=True)
        with open(state_path, "w") as f:
            json.dump(
                {
                    "event_id": event_id,
                    "tasks_passed": 3,
                    "tasks_failed": 0,
                    "total_attempts": 4,
                },
                f,
            )

        print(json.dumps({"event_id": event_id}))
    else:
        # GET .../events/<id> — always terminal-succeeded, immediately.
        print(json.dumps({"status": "succeeded"}))

    return 0


if __name__ == "__main__":
    sys.exit(main())
