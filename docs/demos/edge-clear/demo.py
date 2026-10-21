#!/usr/bin/env python3
"""Show actual serial/batched behavior on disposable native Lance fixtures."""
import json
import os
from pathlib import Path
import subprocess
import sys

root = Path(__file__).resolve().parents[3]
command = ["just", "--command", "cargo", "test", "--features", "knowledge", "batched_edge_clear", "--", "--nocapture"]
environment = {key: value for key, value in os.environ.items() if not key.startswith(("QUIPU_", "BOBBIN_")) and key not in {"GH_TOKEN", "GITHUB_TOKEN"}}
print("Identical synthetic Lance inputs; no production index or model.", flush=True)
print("$ " + " ".join(command), flush=True)
result = subprocess.run(command, cwd=root, env=environment, capture_output=True, text=True, check=False)
if result.returncode:
    print(f"Native reproducer failed (exit {result.returncode}); no success claimed.")
    sys.exit(result.returncode)
rows = [json.loads(line.removeprefix("JXG_RESULT ")) for line in result.stdout.splitlines() if line.startswith("JXG_RESULT ")]
if [row["files"] for row in rows] != [17, 205] or not all(row["rows_equal"] for row in rows):
    raise SystemExit("Native reproducer did not return both complete parity controls")
for row in rows:
    print(json.dumps(row, sort_keys=True))
print("Full payload/repo parity and empty/missing-table/unscoped controls passed.")
