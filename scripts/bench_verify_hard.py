#!/usr/bin/env python3
"""Checker for the local-model-bench HARD tier task: arithmetic expression
evaluator with correct operator precedence and parentheses.

The model under test writes eval_expr.py at the worktree root, defining
evaluate(expr: str) -> float supporting + - * / and parentheses, with
standard precedence (*/  before +-) and left-to-right associativity for
same-precedence operators. Exit 0 on pass, 1 on any failure.
"""
import sys
import importlib.util

spec = importlib.util.spec_from_file_location("eval_expr", "eval_expr.py")
if spec is None or spec.loader is None:
    print("FAIL: eval_expr.py not found or not importable")
    sys.exit(1)
mod = importlib.util.module_from_spec(spec)
try:
    spec.loader.exec_module(mod)
except Exception as e:
    print(f"FAIL: eval_expr.py raised on import: {e}")
    sys.exit(1)

if not hasattr(mod, "evaluate"):
    print("FAIL: eval_expr.py must define evaluate(expr: str) -> float")
    sys.exit(1)

CASES = [
    ("2+3", 5.0),
    ("2+3*4", 14.0),
    ("(2+3)*4", 20.0),
    ("10/2-3", 2.0),
    ("2*(3+4*(5-1))", 38.0),
    ("100", 100.0),
    ("1+2+3+4+5", 15.0),
    ("2*3*4", 24.0),
    ("  2 + 3  ", 5.0),
    ("(1+2)*(3+4)", 21.0),
    ("10-2-3", 5.0),
    ("2.5+2.5", 5.0),
]

failures = []
for expr, expected in CASES:
    try:
        got = mod.evaluate(expr)
    except Exception as e:
        failures.append(f"evaluate({expr!r}) raised {e}")
        continue
    if abs(got - expected) > 1e-6:
        failures.append(f"evaluate({expr!r}) = {got!r}, expected {expected!r}")

if failures:
    print("FAIL:")
    for f in failures:
        print(" -", f)
    sys.exit(1)

print(f"PASS: {len(CASES)} cases")
sys.exit(0)
