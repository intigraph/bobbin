"""Synthetic private artifacts: no model, provider, corpus or eval campaign."""
import json
import shutil
import sqlite3
import subprocess
import time
from pathlib import Path

import pytest

from runner import index_reuse as reuse


def make_repo(path):
    path.mkdir()
    subprocess.run(["git", "init", "-q", str(path)], check=True)
    (path / "app.py").write_text("x = 1\n")
    subprocess.run(["git", "add", "app.py"], cwd=path, check=True)
    subprocess.run(["git", "-c", "user.name=Fixture", "-c", "user.email=f@example.invalid",
                    "commit", "-qm", "private fixture"], cwd=path, check=True)
    return path


def make_index(ws, model):
    root = ws / ".bobbin"
    root.mkdir()
    (root / "config.toml").write_text(
        'quipu_push_chunks = false\nquipu_push_inferred = false\n'
        '[index]\nentities = false\ndocuments = false\nmultimodal = false\n'
        f'[embedding]\nbackend = "onnx"\nmodel = "{model.name}"\ngpu = false\n'
        '[git]\ncoupling_enabled = true\n')
    (root / "quipu").mkdir()
    (root / "quipu/quipu.db").write_bytes(b"synthetic coupling-store fixture")
    (root / "vectors").mkdir()
    (root / "vectors/payload").write_bytes(b"synthetic vectors")
    with sqlite3.connect(root / "index.db") as conn:
        conn.executescript('''
            CREATE TABLE coupling(file_a TEXT,file_b TEXT,score REAL,co_changes INTEGER,
                                  last_co_change INTEGER,PRIMARY KEY(file_a,file_b));
            CREATE TABLE cross_repo_coupling(repo_a TEXT,path_a TEXT,repo_b TEXT,path_b TEXT,
                score REAL,co_changes INTEGER,last_co_change INTEGER,
                PRIMARY KEY(repo_a,path_a,repo_b,path_b));
            CREATE TABLE meta(key TEXT PRIMARY KEY,value TEXT);
            CREATE TABLE file_hashes(repo TEXT,file_path TEXT,hash TEXT);
            INSERT INTO coupling VALUES('app.py','b.py',1,3,1);
            INSERT INTO cross_repo_coupling VALUES('a','app.py','b','b.py',1,3,1);
            INSERT INTO meta VALUES('last_coupling_commit:fixture','bound-head');
            INSERT INTO meta VALUES('coupling_depth:fixture','5000');
            INSERT INTO meta VALUES('last_indexed_commit:fixture','preserved');
            INSERT INTO file_hashes VALUES('fixture','app.py','kept');
        ''')


@pytest.fixture
def bound(tmp_path, monkeypatch):
    monkeypatch.setenv("XDG_CACHE_HOME", str(tmp_path / "cache"))
    model = tmp_path / "cache/bobbin/models/private-fixture"
    (model / "onnx").mkdir(parents=True)
    (model / "tokenizer.json").write_text("{}")
    (model / "onnx/model.onnx").write_bytes(b"synthetic not executable")
    binary = tmp_path / "bobbin"
    binary.write_bytes(b"\x7fELF synthetic binary identity")
    ws = make_repo(tmp_path / "source")
    make_index(ws, model)
    with sqlite3.connect(ws / ".bobbin/index.db") as conn:
        conn.execute("INSERT INTO meta VALUES (?,?)", ("repo_source:" + ws.name, str(ws.resolve())))
    monkeypatch.setitem(reuse.SUPPORTED_RUNTIMES, reuse.digest(binary), {
        "source": "synthetic", "model": model.name,
        "assets": {name: reuse.digest(model / name)
                   for name in ["tokenizer.json", "onnx/model.onnx"]}})
    return ws, binary, model


def test_sealed_positive_and_payload_tamper_falsifier(bound):
    ws, binary, model = bound
    reuse.seal(ws, binary, model, None)
    assert reuse.verify(ws, binary, model, None)["payload"]["bytes"] > 0
    (ws / ".bobbin/vectors/payload").write_bytes(b"corrupt")
    with pytest.raises(reuse.ReuseRefused, match="payload changed"):
        reuse.verify(ws, binary, model, None)


@pytest.mark.parametrize("change", ["source", "config", "model", "binary", "arm",
                                    "stale", "future", "missing", "symlink", "extra"])
def test_unknown_or_changed_identity_never_reuses(bound, change):
    ws, binary, model = bound
    reuse.seal(ws, binary, model, None)
    marker = ws / ".bobbin" / reuse.MARKER
    overrides = None
    if change == "source":
        (ws / "app.py").write_text("x = 2\n")
    elif change == "config":
        with (ws / ".bobbin/config.toml").open("a") as f:
            f.write("coupling_depth = 1\n")
    elif change == "model":
        (model / "onnx/model.onnx").write_bytes(b"another model")
    elif change == "binary":
        binary.write_bytes(b"\x7fELF other executable")
    elif change == "arm":
        overrides = {"git.coupling_enabled": "false"}
    elif change in {"stale", "future"}:
        record = json.loads(marker.read_text())
        record["created"] = time.time() + (60 if change == "future" else -reuse.MAX_AGE - 60)
        marker.write_text(json.dumps(record))
    elif change == "missing":
        (ws / ".bobbin/vectors/payload").unlink()
    elif change == "symlink":
        p = ws / ".bobbin/vectors/payload"
        p.unlink()
        p.symlink_to(binary)
    else:
        (ws / ".bobbin/unexpected").write_text("extra artifact")
    with pytest.raises((reuse.ReuseRefused, OSError)):
        reuse.verify(ws, binary, model, overrides)


def test_no_coupling_copy_preserves_vectors_and_cannot_mutate_full(bound):
    ws, binary, model = bound
    original = reuse.seal(ws, binary, model, None)
    dest = ws.parent / "nocoupling" / ws.name
    shutil.copytree(ws, dest, ignore=shutil.ignore_patterns(".bobbin"))
    result = reuse.derive_no_coupling(ws, dest, binary, model)
    assert result["native_index_calls"] == 0
    assert (ws / ".bobbin/quipu/quipu.db").exists()
    assert not (dest / ".bobbin/quipu").exists()
    reuse.verify(dest, binary, model, {"git.coupling_enabled": "false"})
    with sqlite3.connect(dest / ".bobbin/index.db") as conn:
        assert conn.execute("SELECT count(*) FROM coupling").fetchone() == (0,)
        assert conn.execute("SELECT count(*) FROM cross_repo_coupling").fetchone() == (0,)
        assert conn.execute("SELECT * FROM file_hashes").fetchall() == [("fixture", "app.py", "kept")]
        assert dict(conn.execute("SELECT * FROM meta").fetchall()) == {
            "last_indexed_commit:fixture": "preserved", "repo_source:source": str(dest.resolve())}
    assert reuse.verify(ws, binary, model, None) == original
    full, fork = (p / ".bobbin/vectors/payload" for p in (ws, dest))
    assert full.read_bytes() == fork.read_bytes()
    assert full.stat().st_ino != fork.stat().st_ino
    fork.write_bytes(b"arm-local mutation")
    assert full.read_bytes() == b"synthetic vectors"
    with pytest.raises(reuse.ReuseRefused):
        reuse.verify(dest, binary, model, {"git.coupling_enabled": "false"})


def test_unknown_schema_or_existing_destination_refuses_without_source_change(bound):
    ws, binary, model = bound
    with sqlite3.connect(ws / ".bobbin/index.db") as conn:
        conn.execute("ALTER TABLE coupling ADD COLUMN unknown TEXT")
    record = reuse.seal(ws, binary, model, None)
    dest = ws.parent / "nocoupling" / ws.name
    shutil.copytree(ws, dest, ignore=shutil.ignore_patterns(".bobbin"))
    with pytest.raises(reuse.ReuseRefused, match="schema"):
        reuse.derive_no_coupling(ws, dest, binary, model)
    assert not (dest / ".bobbin").exists()
    assert reuse.verify(ws, binary, model, None) == record
    (dest / ".bobbin").mkdir()
    with pytest.raises(reuse.ReuseRefused, match="already exists"):
        reuse.derive_no_coupling(ws, dest, binary, model)


def test_frozen_matrix_has_1320_unique_keys_and_rejects_duplicates():
    from runner.l0 import ALL_ARMS
    from runner.task_loader import load_all_tasks
    tasks = load_all_tasks(Path(__file__).resolve().parents[1] / "tasks")
    assert len(tasks) == 40
    cells = reuse.planned_cells(tasks, list(ALL_ARMS), [100, 300, 600])
    assert len(cells) == len(set(cells)) == 1320
    with pytest.raises(reuse.ReuseRefused, match="duplicate"):
        reuse.planned_cells(tasks + [tasks[0]], list(ALL_ARMS), [100, 300, 600])


def test_actual_timeout_applies_before_stderr_eof(tmp_path, monkeypatch):
    from runner.bobbin_setup import BobbinSetupError, setup_bobbin
    fake = tmp_path / "bobbin"
    fake.write_text('#!/usr/bin/env python3\nimport sys,time\n'
                    'if sys.argv[1]=="index": time.sleep(2)\n'
                    'if sys.argv[1]=="status": print("{}")\n')
    fake.chmod(0o700)
    monkeypatch.setattr("runner.bobbin_setup._find_bobbin", lambda: str(fake))
    ws = tmp_path / "ws"
    ws.mkdir()
    start = time.monotonic()
    with pytest.raises(BobbinSetupError, match="timed out"):
        setup_bobbin(str(ws), timeout=0.1)
    assert time.monotonic() - start < 1.5
    assert not (ws / ".bobbin" / reuse.MARKER).exists()


def test_actual_cli_callback_derives_once_and_refuses_corrupt_reuse(bound, tmp_path):
    import ast
    ws, binary, model = bound
    shutil.rmtree(ws / ".bobbin")  # exclusively this test's synthetic fixture
    source = Path(__file__).resolve().parents[1] / "runner/cli.py"
    tree = ast.parse(source.read_text())
    command = next(n for n in tree.body if isinstance(n, ast.FunctionDef) and n.name == "l0")
    callback = next(n for n in command.body if isinstance(n, ast.FunctionDef) and n.name == "index")
    calls = []
    def builder(path, **kwargs):
        calls.append(path)
        make_index(Path(path), model)
        with sqlite3.connect(Path(path) / ".bobbin/index.db") as conn:
            conn.execute("INSERT INTO meta VALUES (?,?)", ("repo_source:" + Path(path).name, str(Path(path).resolve())))
        kwargs["before_index"](Path(path))
        return {"index_duration_seconds": 0.01}
    namespace = {"Path": Path, "json": json, "reuse_model_dir": model, "bobbin": str(binary),
                 "scratch": tmp_path, "setup_bobbin": builder, "index_timeout": 30,
                 "derived_sources": {}}
    # Execute only this repository's parsed callback, never fixture/user text.
    exec(compile(ast.Module(body=[callback], type_ignores=[]), str(source), "exec"), namespace)  # noqa: S102
    index = namespace["index"]
    index(ws)
    index(ws)
    dest = ws.parent / "nocoupling" / ws.name
    shutil.copytree(ws, dest, ignore=shutil.ignore_patterns(".bobbin"))
    namespace["derived_sources"][dest] = ws
    index(dest, {"git.coupling_enabled": "false"})
    index(dest, {"git.coupling_enabled": "false"})
    assert len(calls) == 1
    costs = [json.loads(row) for row in (tmp_path / "index-costs.jsonl").read_text().splitlines()]
    assert [row["native_index_calls"] for row in costs] == [1, 0]
    (ws / ".bobbin/vectors/payload").write_bytes(b"tampered")
    with pytest.raises(reuse.ReuseRefused, match="payload changed"):
        index(ws)
    assert len(calls) == 1


def test_timeout_reaps_its_own_index_grandchild(tmp_path, monkeypatch):
    from runner.bobbin_setup import BobbinSetupError, setup_bobbin
    escaped = tmp_path / "escaped"
    child = f'import time,pathlib;time.sleep(0.5);pathlib.Path({str(escaped)!r}).touch()'
    fake = tmp_path / "bobbin"
    fake.write_text('#!/usr/bin/env python3\nimport sys,time,subprocess\n'
                    'if sys.argv[1]=="index":\n'
                    f' subprocess.Popen([sys.executable,"-c",{child!r}])\n'
                    ' time.sleep(2)\n'
                    'if sys.argv[1]=="status": print("{}")\n')
    fake.chmod(0o700)
    monkeypatch.setattr("runner.bobbin_setup._find_bobbin", lambda: str(fake))
    ws = tmp_path / "ws"
    ws.mkdir()
    with pytest.raises(BobbinSetupError, match="timed out"):
        setup_bobbin(str(ws), timeout=0.1)
    time.sleep(0.6)
    assert not escaped.exists()


def test_before_index_admission_failure_never_starts_native_index(tmp_path, monkeypatch):
    from runner.bobbin_setup import setup_bobbin
    monkeypatch.setattr("runner.bobbin_setup._find_bobbin", lambda: "/not-executed")
    monkeypatch.setattr("runner.bobbin_setup.subprocess.run", lambda *a, **k: None)
    def must_not_start(*a, **k):
        pytest.fail("native indexing started before input admission")
    monkeypatch.setattr("runner.bobbin_setup.subprocess.Popen", must_not_start)
    def refuse(path):
        raise reuse.ReuseRefused("model identity missing")
    with pytest.raises(reuse.ReuseRefused, match="model identity missing"):
        setup_bobbin(str(tmp_path), before_index=refuse)


def test_repository_basename_mismatch_refuses(bound):
    ws, binary, model = bound
    reuse.seal(ws, binary, model, None)
    dest = ws.with_name("different-affinity")
    shutil.copytree(ws, dest, ignore=shutil.ignore_patterns(".bobbin"))
    with pytest.raises(reuse.ReuseRefused, match="basename"):
        reuse.derive_no_coupling(ws, dest, binary, model)
    assert not (dest / ".bobbin").exists()


def test_complete_synthetic_matrix_and_partial_error_skip_accounting(tmp_path):
    from runner.l0 import ALL_ARMS, ArmScore
    from runner.task_loader import load_all_tasks
    tasks = load_all_tasks(Path(__file__).resolve().parents[1] / "tasks")
    cells = reuse.planned_cells(tasks, list(ALL_ARMS), [100, 300, 600])
    rows = [ArmScore(t, a, b, ["injected", "error", "skipped"][i % 3])
            for i, (t, a, b) in enumerate(cells)]
    partial = reuse.matrix_coverage(cells, rows[:330])
    assert partial["observed"] == 330 and len(partial["missing"]) == 990
    assert partial["complete"] is False
    full = reuse.matrix_coverage(cells, rows)
    assert full["complete"] and full["observed"] == 1320 and full["missing"] == []
    assert full["outcomes"] == {"injected": 440, "error": 440, "skipped": 440}
    path = tmp_path / "matrix-coverage.json"
    reuse.write_coverage(path, cells, rows)
    assert json.loads(path.read_text()) == full
    with pytest.raises(reuse.ReuseRefused, match="duplicate"):
        reuse.matrix_coverage(cells, rows + [rows[0]])
    with pytest.raises(reuse.ReuseRefused, match="unplanned"):
        reuse.matrix_coverage(cells, [ArmScore("unplanned", "full", 100, "error")])


def test_original_resource_envelope_and_missing_limit_refusals(tmp_path):
    proc = tmp_path / "cgroup"
    proc.write_text("0::/scope\n")
    cg = tmp_path / "sys/scope"
    cg.mkdir(parents=True)
    limits = {"cpu.max": "400000 100000", "memory.max": "6442450944", "memory.swap.max": "0"}
    for key, value in limits.items():
        (cg / key).write_text(value)
    assert reuse.check_scope(proc, tmp_path / "sys") == limits
    (cg / "memory.max").write_text("max")
    with pytest.raises(reuse.ReuseRefused, match="scope"):
        reuse.check_scope(proc, tmp_path / "sys")
    (cg / "memory.max").unlink()
    with pytest.raises(FileNotFoundError):
        reuse.check_scope(proc, tmp_path / "sys")


def test_isolated_cells_restore_history_and_archive_each_cell(bound, tmp_path):
    from runner.cell_isolation import isolated_cell
    from runner.l0 import ArmScore
    ws, binary, model = bound
    original = reuse.seal(ws, binary, model, None)
    def operation():
        root = ws / '.bobbin'
        assert not (root / 'session').exists()
        assert not (root / 'feedback.db').exists()
        (root / 'session').mkdir()
        (root / 'session/ledger.jsonl').write_text('private cell history\n')
        (root / 'feedback.db').write_bytes(b'private feedback')
        return [ArmScore('fixture', 'full', 100, 'injected')]
    for number in range(2):
        journal = tmp_path / f'cell-{number}'
        score = isolated_cell(ws, binary, model, None, journal, operation)
        assert score.outcome == 'injected'
        assert (journal / 'after/session/ledger.jsonl').is_file()
        assert reuse.verify(ws, binary, model, None) == original


@pytest.mark.parametrize('failure', ['immutable', 'error'])
def test_isolated_cell_quarantines_uncertain_writes(bound, tmp_path, failure):
    from runner.cell_isolation import isolated_cell
    from runner.l0 import ArmScore
    ws, binary, model = bound
    reuse.seal(ws, binary, model, None)
    def operation():
        (ws / '.bobbin/hook_state.json').write_text('{}')
        if failure == 'immutable':
            (ws / '.bobbin/vectors/payload').write_bytes(b'changed')
        return [ArmScore('fixture', 'full', 100,
                         'error' if failure == 'error' else 'injected')]
    journal = tmp_path / 'quarantined'
    with pytest.raises(reuse.ReuseRefused):
        isolated_cell(ws, binary, model, None, journal, operation)
    assert (ws / '.bobbin/hook_state.json').exists()
    assert json.loads((journal / 'result.json').read_text())['state'] == 'quarantined'


def test_embedded_coupling_removed_only_from_private_derived_copy(bound):
    ws, binary, model = bound
    reuse.seal(ws, binary, model, None)
    dest = ws.parent / 'derived' / ws.name
    dest.parent.mkdir()
    shutil.copytree(ws, dest, ignore=shutil.ignore_patterns('.bobbin'))
    reuse.derive_no_coupling(ws, dest, binary, model)
    assert (ws / '.bobbin/quipu/quipu.db').read_bytes() == b'synthetic coupling-store fixture'
    assert not (dest / '.bobbin/quipu').exists()
    reuse.verify(ws, binary, model, None)


def test_unknown_embedded_store_layout_refuses_without_source_change(bound):
    ws, binary, model = bound
    (ws / '.bobbin/quipu/unrecognized').write_bytes(b'preserve')
    original = reuse.seal(ws, binary, model, None)
    dest = ws.parent / 'derived' / ws.name
    dest.parent.mkdir()
    shutil.copytree(ws, dest, ignore=shutil.ignore_patterns('.bobbin'))
    with pytest.raises(reuse.ReuseRefused):
        reuse.derive_no_coupling(ws, dest, binary, model)
    assert reuse.verify(ws, binary, model, None) == original
    assert not (dest / '.bobbin').exists()
