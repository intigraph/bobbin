"""Independent-cell contract; fixtures are not new retrieval measurements."""
import csv
import hashlib
import json
from collections import Counter
from pathlib import Path
from subprocess import CompletedProcess

import pytest

from runner import l0

CONTEXT = "--- app.py:1-2 authenticate (function, score 0.91) ---\nx = 1\ny = 2\n"


@pytest.mark.parametrize("stdout,stderr,outcome", [
    (CONTEXT, "", "injected"),
    ("", "bobbin: skipped (operational intent: Operational)", "skipped"),
    ("", "bobbin: skipped (semantic=0.00 < gate=0.45)", "skipped"),
    ("", "bobbin: skipped (semantic=0.45 < gate=0.45)", "skipped"),
    ("", "bobbin: skipped (semantic=0.45 < gate=0.00)", "error"),
    (CONTEXT, "bobbin: injecting 2 new chunks (1 previously injected, turn 2)", "error"),
    ("", "bobbin: skipped (all 2 chunks previously injected, no complementary files)", "error"),
    ("", "bobbin: skipped (session unchanged)", "error"),
    ("", "", "error"),
    ("", "bobbin: skipped (unknown future reason)", "error"),
    (CONTEXT, "bobbin: skipped (semantic=0.00 < gate=0.45)", "error"),
    ("banner without chunk headers", "", "error"),
    (json.dumps({"hookSpecificOutput": {"additionalContext": "## Complementary Files\n- app.py"}}),
     "", "error"),
    ("", "bobbin inject-context: failed to read index", "error"),
    ("", ("bobbin: skipped (semantic=0.00 < gate=0.45)\n"
          "bobbin: skipped (operational intent: Operational)"), "error"),
])
def test_independent_contract(stdout, stderr, outcome):
    assert l0.classify_hook_result(0, stdout, stderr, independent=True)[0] == outcome


def test_legacy_contract_and_original_rows_are_not_reinterpreted():
    assert l0.classify_hook_result(0, "", "")[0] == "skipped"
    reason = "bobbin: skipped (all 2 chunks previously injected, no complementary files)"
    assert l0.classify_hook_result(0, "", reason) == ("skipped", reason)
    root = Path(__file__).resolve().parents[2] / "docs/eval"
    raw = (root / "l0-20261016-rows.csv").read_bytes()
    manifest = json.loads((root / "l0-20261016-manifest.json").read_text())
    assert hashlib.sha256(raw).hexdigest() == manifest["rows_csv_sha256"]
    rows = list(csv.DictReader(raw.decode().splitlines()))
    assert Counter(row["outcome"] for row in rows) == {"injected": 213, "skipped": 83, "error": 34}
    assert sum("previously injected" in row["detail"] for row in rows) == 23
    assert sum(row["detail"] == "injected text had no parseable chunk headers" for row in rows) == 34


@pytest.mark.parametrize("stdout,stderr,outcome", [
    (CONTEXT, "", "injected"),
    ("", "bobbin: skipped (semantic=0.00 < gate=0.45)", "skipped"),
    ("", "bobbin: skipped (all 2 chunks previously injected, no complementary files)", "error"),
    ("", "", "error"),
    ("nonempty unsupported context", "", "error"),
])
def test_driver_propagates_contract_without_numeric_errors(tmp_path, monkeypatch, stdout, stderr, outcome):
    (tmp_path / "app.py").write_text("x = 1\ny = 2\n")
    monkeypatch.setattr(l0, "gold_for_commit", lambda *args: {"app.py": [(1, 2)]})
    calls = []

    def transport(cmd, **kwargs):
        calls.append(cmd)
        assert "--no-dedup" in cmd
        return CompletedProcess(cmd, 0, stdout, stderr)

    monkeypatch.setattr(l0.subprocess, "run", transport)
    row, = l0.run_task({"id": "fixture", "commit": "unused", "description": "authenticate"},
                      tmp_path, ["full"], [300], "not-executed", {},
                      lambda *args: None, strict_hook_contract=True)
    assert len(calls) == 1 and row.outcome == outcome
    if outcome == "error":
        assert row.hunk_recall is None and row.file_recall is None and row.density is None
        assert row.chunks == []
    elif outcome == "injected":
        assert row.hunk_recall == 1 and row.density == 1
    else:
        assert row.hunk_recall == 0  # A registered, explicit gate is a valid non-injection.


def test_nonzero_exit_cannot_be_overridden_by_valid_headers():
    assert l0.classify_hook_result(1, CONTEXT, "", independent=True)[0] == "error"


@pytest.mark.parametrize("contaminated", [False, True])
def test_click_driver_selects_strict_contract_and_quarantines_error(tmp_path, monkeypatch, contaminated):
    """Real CLI -> task -> hook -> classifier -> cell quarantine; fake indexing only."""
    import sys

    from click.testing import CliRunner

    from runner import bobbin_setup, cell_isolation, index_reuse, workspace
    from runner.cli import cli

    binary = tmp_path / "fixture-bobbin"
    output = "" if contaminated else CONTEXT
    error = "bobbin: skipped (all 2 chunks previously injected, no complementary files)" if contaminated else ""
    binary.write_text(f"#!{sys.executable}\nimport sys\n"
                      "if sys.argv[1] == '--version': print('bobbin private fixture')\n"
                      f"else:\n sys.stdin.read()\n sys.stdout.write({output!r})\n sys.stderr.write({error!r})\n")
    binary.chmod(0o700)
    model = tmp_path / "model"
    model.mkdir()
    task = {"id": "fixture", "repo": "fixture/source", "commit": "unused", "description": "authenticate"}
    monkeypatch.setattr("runner.cli.load_task_by_id", lambda *a: task)
    monkeypatch.setattr(bobbin_setup, "_find_bobbin", lambda: str(binary))

    def clone(repo, parent):
        ws = Path(parent) / "fixture--source"
        ws.mkdir(parents=True)
        (ws / "app.py").write_text("x = 1\ny = 2\n")
        return ws

    def index(ws, **kwargs):
        if "before_index" in kwargs:
            kwargs["before_index"](ws)
        (Path(ws) / ".bobbin").mkdir()
        return {}

    empty_seal = {"payload": {"files": {}, "bytes": 0}}
    monkeypatch.setattr(workspace, "clone_repo", clone)
    monkeypatch.setattr(workspace, "checkout_parent", lambda *a: None)
    monkeypatch.setattr(bobbin_setup, "setup_bobbin", index)
    monkeypatch.setattr(index_reuse, "check_scope", lambda: {"fixture": "no campaign"})
    monkeypatch.setattr(index_reuse, "identity", lambda *a: {})
    monkeypatch.setattr(index_reuse, "seal", lambda *a: empty_seal)
    monkeypatch.setattr(index_reuse, "verify", lambda *a: empty_seal)
    monkeypatch.setattr(cell_isolation, "verify", lambda *a: empty_seal)
    monkeypatch.setattr(l0, "gold_for_commit", lambda *a: {"app.py": [(1, 2)]})
    out = tmp_path / "scores.jsonl"
    result = CliRunner().invoke(cli, ["l0", "--task", "fixture", "--arms", "full",
                                     "--budgets", "300", "--out", str(out),
                                     "--workdir", str(tmp_path / "work"),
                                     "--reuse-model-dir", str(model)])
    assert result.exit_code == (1 if contaminated else 0), result.output
    manifest, row = [json.loads(line) for line in out.read_text().splitlines()]
    assert manifest["hook_outcomes"] == l0.INDEPENDENT_HOOK_CONTRACT
    assert row["outcome"] == ("error" if contaminated else "injected")
    if contaminated:
        assert row["hunk_recall"] is None
        journal = tmp_path / "work/cell-state/fixture/full/300/result.json"
        assert json.loads(journal.read_text())["state"] == "quarantined"
    else:
        assert row["hunk_recall"] == 1
