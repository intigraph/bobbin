"""Audit the committed capped cohort without running retrieval or indexing."""
import collections
import csv
import hashlib
import json
from pathlib import Path
import subprocess

BASE = "d619d2212f8dd9784d9936396b36fe9a92f9ae5c"
report = "docs/eval/l0-20261016-partial.md"
before = subprocess.run(["git", "cat-file", "-e", BASE + ":" + report], capture_output=True)
control = subprocess.run(["git", "cat-file", "-e", BASE + ":eval/v2/PREREGISTRATION.md"], capture_output=True)
assert before.returncode != 0 and control.returncode == 0
print("BASE: new partial report absent; preregistration positive control present")
p = Path("docs/eval")
manifest = json.loads((p / "l0-20261016-manifest.json").read_text())
data = (p / "l0-20261016-rows.csv").read_bytes()
assert hashlib.sha256(data).hexdigest() == manifest["rows_csv_sha256"]
rows = list(csv.DictReader(data.decode().splitlines()))
counts = collections.Counter(row["outcome"] for row in rows)
assert len(rows) == 330 and counts == {"injected": 213, "skipped": 83, "error": 34}
assert len({(r["task_id"], r["arm"], r["budget"]) for r in rows}) == 330
assert len({r["task_id"] for r in rows}) == 10
print("CANDIDATE: 330 unique cells, 10/40 tasks, 213 injected / 83 skipped / 34 error")
assert sum(float(r["hunk_recall"]) > 0 for r in rows if r["outcome"] == "injected") == 29
print("POSITIVE CONTROL: 29 injected cells with nonzero hunk recall")
assert hashlib.sha256(bytes([data[0] ^ 1]) + data[1:]).hexdigest() != manifest["rows_csv_sha256"]
print("CORRUPTION CONTROL: one-byte CSV change fails the pinned digest")
assert manifest["full_matrix_complete"] is False
print("INCOMPLETE: no full-cohort efficacy claim; no evaluation or model call executed")
