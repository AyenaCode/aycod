#!/usr/bin/env python3
"""Measures aycod against the budgets of the plan (p50/p95 over N runs, wall clock per process).

Usage: scripts/bench.py <aycod binary> <project dir with aycod.toml, already green> [runs]
Exits 1 if a budget is exceeded. No dependency beyond the Python standard library.
"""

import json
import os
import statistics
import subprocess
import sys
import time

BUDGETS_MS = {"hook": 10.0, "check (cached)": 50.0}


def measure(argv, stdin, cwd, runs, env):
    times = []
    for _ in range(runs):
        start = time.perf_counter_ns()
        p = subprocess.run(argv, input=stdin, cwd=cwd, env=env, capture_output=True)
        times.append((time.perf_counter_ns() - start) / 1e6)
        if p.returncode not in (0, 2):
            sys.exit(f"{argv} failed ({p.returncode}): {p.stderr.decode()}")
    times.sort()
    return statistics.median(times), times[int(len(times) * 0.95) - 1]


def main():
    binary, project = os.path.abspath(sys.argv[1]), os.path.abspath(sys.argv[2])
    runs = int(sys.argv[3]) if len(sys.argv) > 3 else 200
    env = {**os.environ, "PATH": os.path.dirname(binary) + os.pathsep + os.environ["PATH"]}
    env.pop("AYCOD_GATE", None)
    bash = lambda c: json.dumps({"tool_name": "Bash", "tool_input": {"command": c}, "cwd": project}).encode()
    edit = json.dumps({"tool_name": "Edit", "tool_input": {"file_path": f"{project}/src/app.ts"}, "cwd": project})
    cases = [
        ("startup (--version)", [binary, "--version"], b"", None),
        ("hook: allowed command", [binary, "hook", "claude", "pre-tool"], bash("pnpm exec vitest run 2>&1 | tail -n 50"), "hook"),
        ("hook: denied command", [binary, "hook", "claude", "pre-tool"], bash("git push --force origin feat"), "hook"),
        ("hook: edit", [binary, "hook", "claude", "pre-tool"], edit.encode(), "hook"),
        ("check (cached)", [binary, "check"], b"", "check (cached)"),
        ("hook: stop (cached)", [binary, "hook", "claude", "stop"], json.dumps({"cwd": project}).encode(), "check (cached)"),
    ]
    subprocess.run([binary, "check"], cwd=project, env=env, check=True, capture_output=True)
    failed = False
    print(f"{'case':<26}{'p50 ms':>9}{'p95 ms':>9}  budget")
    for name, argv, stdin, budget in cases:
        p50, p95 = measure(argv, stdin, project, runs, env)
        limit = BUDGETS_MS.get(budget)
        verdict = "" if limit is None else (f"< {limit:g} ms ✓" if p95 < limit else f"< {limit:g} ms ✗")
        failed |= limit is not None and p95 >= limit
        print(f"{name:<26}{p50:>9.2f}{p95:>9.2f}  {verdict}")
    size = os.path.getsize(binary) / 1e6
    print(f"{'binary size':<26}{size:>8.2f} MB  < 15 MB {'✓' if size < 15 else '✗'}")
    sys.exit(1 if failed or size >= 15 else 0)


if __name__ == "__main__":
    main()
