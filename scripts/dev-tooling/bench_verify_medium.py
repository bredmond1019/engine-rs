#!/usr/bin/env python3
"""Checker for the local-model-bench MEDIUM tier task: run-length encode/decode.

The model under test writes rle.py at the worktree root, defining
encode(s: str) -> str and decode(s: str) -> str. This script imports it and
checks explicit expected outputs (not just round-trip) so a model can't pass
by decoding its own wrong encoding. Exit 0 on pass, 1 on any failure.
"""
import sys
import importlib.util

spec = importlib.util.spec_from_file_location("rle", "rle.py")
if spec is None or spec.loader is None:
    print("FAIL: rle.py not found or not importable")
    sys.exit(1)
mod = importlib.util.module_from_spec(spec)
try:
    spec.loader.exec_module(mod)
except Exception as e:
    print(f"FAIL: rle.py raised on import: {e}")
    sys.exit(1)

if not hasattr(mod, "encode") or not hasattr(mod, "decode"):
    print("FAIL: rle.py must define encode(s) and decode(s)")
    sys.exit(1)

CASES = [
    ("", ""),
    ("a", "1a"),
    ("aaabbbcc", "3a3b2c"),
    ("abcd", "1a1b1c1d"),
    ("aaaaaaaaaa", "10a"),
    ("aabbaa", "2a2b2a"),
]

failures = []
for original, expected_encoded in CASES:
    try:
        got_encoded = mod.encode(original)
    except Exception as e:
        failures.append(f"encode({original!r}) raised {e}")
        continue
    if got_encoded != expected_encoded:
        failures.append(f"encode({original!r}) = {got_encoded!r}, expected {expected_encoded!r}")
        continue
    try:
        got_decoded = mod.decode(got_encoded)
    except Exception as e:
        failures.append(f"decode({got_encoded!r}) raised {e}")
        continue
    if got_decoded != original:
        failures.append(f"decode(encode({original!r})) = {got_decoded!r}, expected {original!r}")

if failures:
    print("FAIL:")
    for f in failures:
        print(" -", f)
    sys.exit(1)

print(f"PASS: {len(CASES)} cases")
sys.exit(0)
