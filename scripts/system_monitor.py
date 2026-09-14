#!/usr/bin/env python3
"""Sample system CPU/memory/swap during the local-model bench sweep, keep a
rolling average, and fire a `bastion notify send` Telegram alert on a
sustained, edge-triggered CPU/memory/swap problem — never per-sample noise.

No new dependency: reads macOS `top -l 1`, `sysctl vm.swapusage`, and
`memory_pressure` directly rather than pulling in `psutil`.

Thresholds are config, not hardcoded literals (CLAUDE.md standing rule: a
value trading cost/safety/quality is a knob) -- every one of them is a CLI
flag with a documented default; see --help. Defaults were chosen for a
32GB M-series Mac Mini running local Ollama models + the bench, not derived
from any measurement -- tighten or loosen them once real data comes in.
"""

from __future__ import annotations

import argparse
import json
import re
import subprocess
import sys
import time
from collections import deque
from dataclasses import dataclass, field
from datetime import datetime, timezone
from pathlib import Path

BASTION_DIR = Path(__file__).resolve().parents[2] / "bastion"


@dataclass
class Sample:
    ts: str
    cpu_busy_pct: float  # 100 - idle, from `top -l 1`'s "CPU usage" line
    mem_free_pct: float  # `memory_pressure`'s "System-wide memory free percentage"
    swap_used_gb: float  # `sysctl vm.swapusage`'s "used" field


@dataclass
class Severity:
    # Ordered so max()/comparisons work: NONE < WARN < CRIT.
    order: int
    name: str

    def __lt__(self, other: "Severity") -> bool:
        return self.order < other.order


NONE = Severity(0, "none")
WARN = Severity(1, "WARN")
CRIT = Severity(2, "CRIT")


def run_top_cpu_busy_pct() -> float:
    """`top -l 1`'s "CPU usage: X% user, Y% sys, Z% idle" line -> 100 - idle."""
    out = subprocess.run(
        ["top", "-l", "1", "-n", "0", "-s", "0"],
        capture_output=True, text=True, timeout=15, check=True,
    ).stdout
    m = re.search(r"CPU usage:\s*([\d.]+)%\s*user,\s*([\d.]+)%\s*sys,\s*([\d.]+)%\s*idle", out)
    if not m:
        raise RuntimeError(f"could not parse `top` CPU line from: {out[:200]!r}")
    idle = float(m.group(3))
    return round(100.0 - idle, 1)


def run_memory_free_pct() -> float:
    """`memory_pressure`'s own "System-wide memory free percentage: N%" line."""
    out = subprocess.run(
        ["memory_pressure"], capture_output=True, text=True, timeout=15, check=True,
    ).stdout
    m = re.search(r"System-wide memory free percentage:\s*(\d+)%", out)
    if not m:
        raise RuntimeError(f"could not parse `memory_pressure` output: {out[:200]!r}")
    return float(m.group(1))


def run_swap_used_gb() -> float:
    """`sysctl vm.swapusage`'s "used = NNN.NNM" (or G) field, normalized to GB."""
    out = subprocess.run(
        ["sysctl", "vm.swapusage"], capture_output=True, text=True, timeout=15, check=True,
    ).stdout
    m = re.search(r"used\s*=\s*([\d.]+)([MG])", out)
    if not m:
        raise RuntimeError(f"could not parse `sysctl vm.swapusage` output: {out!r}")
    value, unit = float(m.group(1)), m.group(2)
    return round(value / 1024.0 if unit == "M" else value, 2)


def take_sample() -> Sample:
    return Sample(
        ts=datetime.now(timezone.utc).isoformat(timespec="seconds"),
        cpu_busy_pct=run_top_cpu_busy_pct(),
        mem_free_pct=run_memory_free_pct(),
        swap_used_gb=run_swap_used_gb(),
    )


def classify(avg_cpu: float, avg_mem_free: float, avg_swap_gb: float, args: argparse.Namespace) -> tuple[Severity, list[str]]:
    """Rolling-average thresholds -> (worst severity, human-readable reasons)."""
    reasons: list[str] = []
    worst = NONE

    if avg_cpu >= args.cpu_crit_pct:
        worst = max(worst, CRIT, key=lambda s: s.order)
        reasons.append(f"CPU busy {avg_cpu:.1f}% (avg) >= critical {args.cpu_crit_pct}%")
    elif avg_cpu >= args.cpu_warn_pct:
        worst = max(worst, WARN, key=lambda s: s.order)
        reasons.append(f"CPU busy {avg_cpu:.1f}% (avg) >= warn {args.cpu_warn_pct}%")

    if avg_mem_free <= args.mem_free_crit_pct:
        worst = max(worst, CRIT, key=lambda s: s.order)
        reasons.append(f"memory free {avg_mem_free:.1f}% (avg) <= critical {args.mem_free_crit_pct}%")
    elif avg_mem_free <= args.mem_free_warn_pct:
        worst = max(worst, WARN, key=lambda s: s.order)
        reasons.append(f"memory free {avg_mem_free:.1f}% (avg) <= warn {args.mem_free_warn_pct}%")

    if avg_swap_gb >= args.swap_crit_gb:
        worst = max(worst, CRIT, key=lambda s: s.order)
        reasons.append(f"swap used {avg_swap_gb:.2f}GB (avg) >= critical {args.swap_crit_gb}GB")
    elif avg_swap_gb >= args.swap_warn_gb:
        worst = max(worst, WARN, key=lambda s: s.order)
        reasons.append(f"swap used {avg_swap_gb:.2f}GB (avg) >= warn {args.swap_warn_gb}GB")

    return worst, reasons


def send_telegram(text: str) -> None:
    try:
        subprocess.run(
            ["bastion", "notify", "send", "--text", text],
            cwd=BASTION_DIR, capture_output=True, text=True, timeout=30, check=True,
        )
    except Exception as exc:  # noqa: BLE001 -- never let a notify failure kill the monitor
        print(f"[monitor] bastion notify send failed: {exc}", file=sys.stderr)


def top_processes(n: int = 5) -> list[str]:
    out = subprocess.run(
        ["ps", "aux"], capture_output=True, text=True, timeout=15, check=True,
    ).stdout.splitlines()[1:]
    rows = sorted(out, key=lambda l: -_safe_float(l.split(None, 10), 2))[:n]
    lines = []
    for row in rows:
        parts = row.split(None, 10)
        if len(parts) < 11:
            continue
        lines.append(f"{parts[2]}% cpu / {parts[3]}% mem  {parts[10][:60]}")
    return lines


def _safe_float(parts: list[str], idx: int) -> float:
    try:
        return float(parts[idx])
    except (IndexError, ValueError):
        return 0.0


def main() -> None:
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--interval-seconds", type=float, default=60.0, help="seconds between samples (default: 60)")
    p.add_argument("--window", type=int, default=10, help="rolling-average window in samples (default: 10 = 10min @ 60s)")
    p.add_argument("--cpu-warn-pct", type=float, default=85.0, help="rolling-avg system CPU busy%% for WARN (default: 85)")
    p.add_argument("--cpu-crit-pct", type=float, default=95.0, help="rolling-avg system CPU busy%% for CRIT (default: 95)")
    p.add_argument("--mem-free-warn-pct", type=float, default=10.0, help="rolling-avg free-mem%% at/below which is WARN (default: 10)")
    p.add_argument("--mem-free-crit-pct", type=float, default=5.0, help="rolling-avg free-mem%% at/below which is CRIT (default: 5)")
    p.add_argument("--swap-warn-gb", type=float, default=4.0, help="rolling-avg swap-used GB for WARN (default: 4)")
    p.add_argument("--swap-crit-gb", type=float, default=8.0, help="rolling-avg swap-used GB for CRIT (default: 8)")
    p.add_argument("--renotify-minutes", type=float, default=30.0, help="minimum minutes between repeat alerts while still elevated (default: 30)")
    p.add_argument("--log", type=Path, default=Path("/tmp/system_monitor.jsonl"), help="JSONL sample log path")
    p.add_argument("--once", action="store_true", help="take exactly one sample, print it, and exit (for smoke-testing)")
    args = p.parse_args()

    window: deque[Sample] = deque(maxlen=args.window)
    state = NONE
    last_notify: float = 0.0

    while True:
        try:
            s = take_sample()
        except Exception as exc:  # noqa: BLE001 -- a transient sampling error must not kill the monitor
            print(f"[monitor] sample failed: {exc}", file=sys.stderr)
            if args.once:
                sys.exit(1)
            time.sleep(args.interval_seconds)
            continue

        window.append(s)
        avg_cpu = sum(x.cpu_busy_pct for x in window) / len(window)
        avg_mem_free = sum(x.mem_free_pct for x in window) / len(window)
        avg_swap = sum(x.swap_used_gb for x in window) / len(window)
        severity, reasons = classify(avg_cpu, avg_mem_free, avg_swap, args)

        record = {
            "ts": s.ts,
            "cpu_busy_pct": s.cpu_busy_pct,
            "mem_free_pct": s.mem_free_pct,
            "swap_used_gb": s.swap_used_gb,
            "avg_cpu_busy_pct": round(avg_cpu, 1),
            "avg_mem_free_pct": round(avg_mem_free, 1),
            "avg_swap_used_gb": round(avg_swap, 2),
            "window": len(window),
            "severity": severity.name,
            "reasons": reasons,
        }
        with args.log.open("a") as f:
            f.write(json.dumps(record) + "\n")
        print(json.dumps(record))

        now = time.monotonic()
        escalated = severity.order > state.order
        recovered = severity is NONE and state is not NONE
        due_for_renotify = severity.order > 0 and (now - last_notify) >= args.renotify_minutes * 60

        if severity.order > 0 and len(window) == window.maxlen and (escalated or due_for_renotify):
            procs = "\n".join(top_processes())
            text = (
                f"[bench-monitor] {severity.name}: " + "; ".join(reasons) +
                f"\n(rolling avg over last {len(window)} samples)"
                f"\nTop processes:\n{procs}"
            )
            send_telegram(text)
            last_notify = now
        elif recovered and len(window) == window.maxlen:
            send_telegram(
                f"[bench-monitor] recovered: CPU {avg_cpu:.1f}%, mem free {avg_mem_free:.1f}%, "
                f"swap {avg_swap:.2f}GB — back under warn thresholds"
            )
            last_notify = now

        state = severity

        if args.once:
            return
        time.sleep(args.interval_seconds)


if __name__ == "__main__":
    main()
