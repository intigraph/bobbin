"""Isolate known hook state for each L0 cell without copying immutable vectors.

Only the hash-enrolled runtime is supported. Every other index byte must stay
unchanged. Uncertain execution or restoration quarantines the workspace and stops
this run; no following cell can consume it. Journals are private run artifacts.
"""
from __future__ import annotations

import json
import os
import shutil
from collections.abc import Callable
from dataclasses import asdict
from pathlib import Path

from runner.index_reuse import MARKER, ReuseRefused, inventory, verify

# Native hook/session writes observed and source-audited in runtime 843dc42.
MUTABLE = frozenset({
    "calibration.json", "calibration_progress.jsonl", "metrics.jsonl",
    "hook_state.json", "feedback.db", "feedback.db-wal", "feedback.db-shm",
    "session", "hot-topics.md",
})
HISTORY = {"hook_state.json", "feedback.db", "session", "hot-topics.md"}


def _copy(source: Path, target: Path) -> None:
    if source.is_symlink():
        raise ReuseRefused("linked mutable state")
    if source.is_dir():
        shutil.copytree(source, target, copy_function=shutil.copy2)
    else:
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(source, target)


def isolated_cell(ws: Path, binary: Path, model: Path, overrides: dict | None,
                  journal: Path, operation: Callable) -> object:
    """Execute exactly one score operation, archive writes, restore sealed state."""
    original = verify(ws, binary, model, overrides)
    root = ws / ".bobbin"
    if any(name.split("/", 1)[0] in HISTORY for name in original["payload"]["files"]):
        raise ReuseRefused("sealed index already contains hook history")
    journal.mkdir(parents=True, exist_ok=False)
    before, after = journal / "before", journal / "after"
    before.mkdir()
    after.mkdir()
    for name in MUTABLE:
        p = root / name
        if p.exists():
            _copy(p, before / name)
    score = None
    try:
        scores = operation()
        if len(scores) != 1:
            raise ReuseRefused("isolated operation must produce exactly one cell")
        score = scores[0]
        # An error can represent a timeout with a surviving/unknown writer.
        # Never reset its files and let a later cell run on that uncertainty.
        if score.outcome == "error":
            raise ReuseRefused("native cell error; workspace quarantined")
        observed = inventory(root, exclude=MARKER)
        old, new = original["payload"]["files"], observed["files"]
        changed = {key for key in old.keys() | new.keys() if old.get(key) != new.get(key)}
        if any(key.split("/", 1)[0] not in MUTABLE for key in changed):
            raise ReuseRefused("native cell changed immutable index payload")
        # Move rather than discard all cell state, including session ledger and
        # feedback. Never touch the source/model/vector data or a prior journal.
        for name in MUTABLE:
            current = root / name
            if current.exists():
                os.rename(current, after / name)
            saved = before / name
            if saved.exists():
                _copy(saved, current)
        if verify(ws, binary, model, overrides) != original:
            raise ReuseRefused("cell restoration did not reconstruct sealed state")
        result = {"state": "restored", "score": asdict(score),
                  "changed_files": sorted(changed)}
    except BaseException as exc:
        result = {"state": "quarantined", "error": type(exc).__name__,
                  "score": asdict(score) if score is not None else None}
        (journal / "result.json").write_text(json.dumps(result, sort_keys=True) + "\n")
        raise
    with (journal / "result.json").open("x") as f:
        json.dump(result, f, sort_keys=True)
        f.flush()
        os.fsync(f.fileno())
    return score
