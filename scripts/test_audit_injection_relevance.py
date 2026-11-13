"""Paired provenance and UNKNOWN controls for the telemetry audit."""
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest
from datetime import datetime, timezone

spec = importlib.util.spec_from_file_location("audit", Path(__file__).with_name("audit-injection-relevance.py"))
audit = importlib.util.module_from_spec(spec)
spec.loader.exec_module(audit)


class AuditTest(unittest.TestCase):
    def test_provenance_deduplicates_and_legacy_remains_unknown(self):
        with tempfile.TemporaryDirectory() as root:
            path = Path(root) / "worker" / ".bobbin" / "metrics.jsonl"
            path.parent.mkdir(parents=True)
            events = []
            for identity in ("one", "two"):
                events.append({"timestamp": "2026-01-02T00:00:00Z", "event_type": "hook_injection", "metadata": {
                    "injection_id": identity, "file_provenance": [{"repo": "engine", "path": "src/main.rs", "chunks": ["key"]}]}})
            events.append({"timestamp": "2026-01-02T00:00:00Z", "event_type": "hook_injection", "metadata": {"files_returned": ["src/legacy.rs"]}})
            path.write_text("\n".join(json.dumps(e) for e in events) + "\n")
            result = audit.audit([path], datetime(2026, 1, 1, tzinfo=timezone.utc), datetime(2026, 1, 3, tzinfo=timezone.utc))
            by_repo = {r["repo"]: r for r in result["rows"]}
            self.assertEqual(by_repo["engine"]["injections"], 2)
            self.assertEqual(by_repo["engine"]["distinct_chunks"], 1)
            self.assertIsNone(by_repo["UNKNOWN"]["distinct_chunks"])
            self.assertIsNone(by_repo["UNKNOWN"]["injections"])
            self.assertIsNone(by_repo["engine"]["usage"])
            self.assertTrue(result["coverage"][0]["complete"])


if __name__ == "__main__":
    unittest.main()
