#!/usr/bin/env python3
"""Audit bounded injection telemetry without inferring missing repo or usage data."""
import argparse
from datetime import datetime
import json
from pathlib import Path


def audit(paths, since, until):
    rows = {}
    coverage = []
    for raw_path in paths:
        path = Path(raw_path)
        agent = path.parent.parent.name
        if path.stat().st_size > 64 * 1024 * 1024:
            coverage.append({"file": str(path), "complete": False, "reason": "64MiB cap"})
            continue
        bad = 0
        with path.open(encoding="utf-8") as stream:
            for index, line in enumerate(stream):
                if index >= 500000:
                    break
                try:
                    event = json.loads(line)
                    at = datetime.fromisoformat(event["timestamp"].replace("Z", "+00:00"))
                    if not since <= at <= until or event.get("event_type") != "hook_injection":
                        continue
                    meta = event.get("metadata", {})
                    identity = meta.get("injection_id")
                    provenance = meta.get("file_provenance")
                    if not isinstance(provenance, list):
                        provenance = [{"path": p, "repo": None} for p in meta.get("files_returned", [])]
                    for file in provenance:
                        if not isinstance(file, dict) or not isinstance(file.get("path"), str):
                            bad += 1
                            continue
                        repo = file.get("repo")
                        if not isinstance(repo, str) or not repo:
                            repo = "UNKNOWN"
                        key = (agent, repo)
                        row = rows.setdefault(key, {"injections": set(), "paths": set(), "chunks": set(), "missing_chunks": False, "missing_identity": False})
                        if isinstance(identity, str):
                            row["injections"].add(identity)
                        else:
                            row["missing_identity"] = True
                        row["paths"].add(file["path"])
                        chunks = file.get("chunks")
                        if isinstance(chunks, list) and all(isinstance(c, str) for c in chunks):
                            row["chunks"].update(chunks)
                        else:
                            row["missing_chunks"] = True
                except (ValueError, KeyError, TypeError, AttributeError):
                    bad += 1
            else:
                coverage.append({"file": str(path), "complete": True, "malformed": bad})
                continue
            coverage.append({"file": str(path), "complete": False, "reason": "500000 record cap", "malformed": bad})
    return {"scope": "supplied metric files only; missing provenance, ratings and usage are UNKNOWN", "coverage": coverage,
            "rows": [{"agent": a, "repo": r, "injections": None if v["missing_identity"] else len(v["injections"]), "distinct_files": len(v["paths"]),
                      "distinct_chunks": None if v["missing_chunks"] else len(v["chunks"]),
                      "ratings": None, "usage": None} for (a, r), v in sorted(rows.items())]}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("metrics", nargs="+")
    parser.add_argument("--since", required=True, help="inclusive ISO8601 timestamp with timezone")
    parser.add_argument("--until", required=True, help="inclusive ISO8601 timestamp with timezone")
    args = parser.parse_args()
    since, until = (datetime.fromisoformat(s.replace("Z", "+00:00")) for s in (args.since, args.until))
    if since.tzinfo is None or until.tzinfo is None or since > until:
        parser.error("ordered timezone-aware timestamps required")
    print(json.dumps(audit(args.metrics, since, until), indent=2))


if __name__ == "__main__":
    main()
