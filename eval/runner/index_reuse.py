"""Opt-in, run-local L0 index reuse. Never opens a production index.

A no-coupling arm gets an independent copy of a sealed full index: vectors are
unchanged, both coupling tables and their watermarks are removed, and the sole
config change disables coupling. This is not incremental reuse across commits.
Unknown schema/model/config, mutable artifacts or stale seals refuse.
"""
from __future__ import annotations

import hashlib
import json
import os
import shutil
import sqlite3
import stat
import subprocess
import tempfile
import time
import tomllib
from contextlib import closing
from pathlib import Path

from runner.bobbin_setup import apply_config_overrides

MARKER = "eval-index-reuse.json"
# Validated against the frozen L0 runtime, not inferred from a version label.
# Enroll another build only with paired native cold/derived controls.
SUPPORTED_RUNTIMES = {
    "9bebd2fad732dc20ae614d6d2df6829d480e5306b0cc6a7b28653c7b0ec69721": {
        "source": "843dc42bae0cfd39718d29eaecb3b12fe6e8dbfe",
        "model": "all-MiniLM-L6-v2",
        "assets": {
            "tokenizer.json": "da0e79933b9ed51798a3ae27893d3c5fa4a201126cef75586296df9b4d2c62a0",  # gitleaks:allow public model SHA256, not a credential
            "onnx/model.onnx": "759c3cd2b7fe7e93933ad23c4c9181b7396442a2ed746ec7c1d46192c469c46e",
        },
    },
}
MAX_AGE = 21600
MAX_BYTES = 512 * 1024**2


class ReuseRefused(RuntimeError):
    """Identity or artifact could not be proved; no fallback rebuild."""


def digest(path: Path) -> str:
    if not stat.S_ISREG(path.lstat().st_mode):
        raise ReuseRefused(f"not a regular file: {path.name}")
    h = hashlib.sha256()
    with path.open("rb") as f:
        for part in iter(lambda: f.read(1024 * 1024), b""):
            h.update(part)
    return h.hexdigest()


def inventory(root: Path, *, exclude: str | None = None) -> dict:
    if not stat.S_ISDIR(root.lstat().st_mode):
        raise ReuseRefused("inventory root must be a real directory")
    files, total = {}, 0
    for base, dirs, names in os.walk(root, followlinks=False):
        for name in dirs:
            if not stat.S_ISDIR((Path(base) / name).lstat().st_mode):
                raise ReuseRefused("linked directory in artifact")
        for name in sorted(names):
            p = Path(base) / name
            rel = p.relative_to(root).as_posix()
            if rel == exclude:
                continue
            size = p.lstat().st_size
            total += size
            if total > MAX_BYTES or len(files) >= 100000:
                raise ReuseRefused("artifact inventory bound exceeded")
            files[rel] = {"size": size, "sha256": digest(p)}
    if not files:
        raise ReuseRefused("empty inventory")
    return {"files": files, "bytes": total}


def git(ws: Path, *args: str) -> str:
    return subprocess.check_output(["git", *args], cwd=ws, timeout=30).decode().strip()


def source_identity(ws: Path) -> dict:
    raw = subprocess.check_output(
        ["git", "ls-files", "-c", "-o", "--exclude-standard", "-z"], cwd=ws, timeout=30
    )
    files = {}
    for name in sorted(set(raw.decode().split("\0")) - {""}):
        if name.split("/", 1)[0] == ".bobbin":
            continue
        # Indexed source may not escape through a symlink, including parents.
        p = ws
        for part in Path(name).parts:
            p = p / part
            if p.is_symlink():
                raise ReuseRefused("linked source file")
        files[name] = {"sha256": digest(p), "mode": stat.S_IMODE(p.stat().st_mode)}
    if not files:
        raise ReuseRefused("empty source identity")
    return {"head": git(ws, "rev-parse", "HEAD"), "files": files}


def identity(ws: Path, binary: Path, model: Path, overrides: dict | None) -> dict:
    with binary.open("rb") as executable:
        if executable.read(4) != b"\x7fELF":
            raise ReuseRefused("reuse requires a directly pinned native ELF, not an executable wrapper")
    binary_sha = digest(binary)
    runtime = SUPPORTED_RUNTIMES.get(binary_sha)
    if runtime is None or runtime["model"] != model.name:
        raise ReuseRefused("native runtime/model has no paired derivation proof")
    config = ws / ".bobbin/config.toml"
    cfg = tomllib.loads(config.read_text())
    if (cfg.get("quipu_push_chunks") is not False
            or cfg.get("quipu_push_inferred") is not False
            or any(cfg.get("index", {}).get(key) is not False
                   for key in ("entities", "documents", "multimodal"))):
        raise ReuseRefused("only the frozen non-publishing, code-only index profile is supported")
    embedding = cfg.get("embedding", {})
    if (embedding.get("backend") != "onnx" or embedding.get("gpu") is not False
            or embedding.get("model") != model.name):
        raise ReuseRefused("reuse supports only the exact named CPU ONNX model directory")
    expected = Path(os.environ.get("XDG_CACHE_HOME", "")) / "bobbin/models" / model.name
    if not os.environ.get("XDG_CACHE_HOME") or model.resolve() != expected.resolve():
        raise ReuseRefused("model is not the isolated native cache selected by XDG_CACHE_HOME")
    assets = inventory(model)
    if not {"tokenizer.json", "onnx/model.onnx"} <= assets["files"].keys():
        raise ReuseRefused("missing ONNX/tokenizer model identity")
    if any(assets["files"][name]["sha256"] != expected
           for name, expected in runtime["assets"].items()):
        raise ReuseRefused("runtime's pinned model asset changed")
    return {"source": source_identity(ws), "binary": binary_sha,
            "config": digest(config), "models": assets, "overrides": overrides}


def seal(ws: Path, binary: Path, model: Path, overrides: dict | None) -> dict:
    root = ws / ".bobbin"
    if not (root / "index.db").is_file() or not (root / "vectors").is_dir():
        raise ReuseRefused("missing native SQLite/vector index")
    record = {"format": "l0-index-reuse/v1", "created": time.time(),
              "identity": identity(ws, binary, model, overrides),
              "payload": inventory(root, exclude=MARKER)}
    target = root / MARKER
    if target.exists():
        raise ReuseRefused("seal already exists")
    with target.open("x") as f:
        json.dump(record, f, sort_keys=True)
        f.flush()
        os.fsync(f.fileno())
    return record


def verify(ws: Path, binary: Path, model: Path, overrides: dict | None) -> dict:
    root = ws / ".bobbin"
    marker = root / MARKER
    digest(marker)  # lstat refuses symlinks, unreadable/missing is not a cache miss.
    record = json.loads(marker.read_text())
    if record.get("format") != "l0-index-reuse/v1":
        raise ReuseRefused("unknown index seal")
    age = time.time() - record["created"]
    if not 0 <= age <= MAX_AGE:
        raise ReuseRefused("stale or future index seal")
    if record["identity"] != identity(ws, binary, model, overrides):
        raise ReuseRefused("source/config/binary/model/arm identity mismatch")
    if record["payload"] != inventory(root, exclude=MARKER):
        raise ReuseRefused("index payload changed or incomplete")
    return record


def derive_no_coupling(source: Path, dest: Path, binary: Path, model: Path) -> dict:
    """Copy closed private artifacts, transform coupling only, verify before publish.

    Call before any hook/query mutates the source index. Both workspaces must be
    exclusively owned by this sequential run. Uncooperative external mutation is
    unsupported; pre/post inventories detect observed changes, not filesystem CAS.
    """
    if source.name != dest.name:
        raise ReuseRefused("repository basename/affinity must be identical in isolated arms")
    original = verify(source, binary, model, None)
    if source_identity(dest) != original["identity"]["source"]:
        raise ReuseRefused("destination source does not match sealed checkout")
    target = dest / ".bobbin"
    if target.exists() or target.is_symlink():
        raise ReuseRefused("destination index already exists")
    if shutil.disk_usage(dest).free < original["payload"]["bytes"] + 16 * 1024**2:
        raise ReuseRefused("insufficient disk for independent artifact copy")
    start = time.monotonic()
    staging = Path(tempfile.mkdtemp(prefix=".eval-index-", dir=dest))
    try:
        copy = staging / ".bobbin"
        shutil.copytree(source / ".bobbin", copy, copy_function=shutil.copy2)
        (copy / MARKER).unlink()
        db = copy / "index.db"
        with closing(sqlite3.connect(db)) as conn:
            expected = {
                "coupling": ["file_a", "file_b", "score", "co_changes", "last_co_change"],
                "cross_repo_coupling": ["repo_a", "path_a", "repo_b", "path_b", "score",
                                        "co_changes", "last_co_change"],
                "meta": ["key", "value"],
            }
            for table, columns in expected.items():
                actual = [row[1] for row in conn.execute(f"PRAGMA table_info({table})")]
                if actual != columns:
                    raise ReuseRefused("unsupported coupling metadata schema")
            if conn.execute("PRAGMA integrity_check").fetchone() != ("ok",):
                raise ReuseRefused("invalid copied SQLite index")
            key = "repo_source:" + source.name
            rows = conn.execute("SELECT value FROM meta WHERE key=?", (key,)).fetchall()
            if rows != [(str(source.resolve()),)]:
                raise ReuseRefused("native repository source binding is missing or changed")
            conn.execute("UPDATE meta SET value=? WHERE key=?", (str(dest.resolve()), key))
            conn.execute("DELETE FROM coupling")
            conn.execute("DELETE FROM cross_repo_coupling")
            conn.execute("DELETE FROM meta WHERE key LIKE 'last_coupling_commit:%' "
                         "OR key LIKE 'coupling_depth:%'")
            conn.commit()
        # This exact runtime also writes coupling edges to its default embedded
        # Quipu store, independently of the HTTP publication flags. Cold
        # no-coupling indexing creates no such store in the supported profile.
        knowledge = copy / "quipu"
        if knowledge.exists():
            names = {p.relative_to(copy).as_posix() for p in knowledge.rglob("*") if p.is_file()}
            if names != {"quipu/quipu.db"}:
                raise ReuseRefused("unknown embedded coupling-store layout")
            shutil.rmtree(knowledge)  # only this newly generated private copy
        apply_config_overrides(str(staging), {"git.coupling_enabled": "false"})
        # Only the config and SQLite metadata may change. All vector/other bytes
        # must still equal the original closed artifact; no hard links are used.
        copied = inventory(copy)
        for rel, entry in original["payload"]["files"].items():
            if (rel not in {"index.db", "config.toml", "quipu/quipu.db"}
                    and copied["files"].get(rel) != entry):
                raise ReuseRefused("non-coupling payload changed during derivation")
        verify(source, binary, model, None)
        os.rename(copy, target)  # caller's private fresh workspace, no other writers
        # Failure leaves the destination intact; no silent rebuild/retry.
        sealed = seal(dest, binary, model, {"git.coupling_enabled": "false"})
        return {"kind": "derived-no-coupling", "seconds": time.monotonic() - start,
                "copied_bytes": original["payload"]["bytes"],
                "payload_bytes": sealed["payload"]["bytes"], "native_index_calls": 0}
    finally:
        # Only this newly created temporary staging directory is disposable.
        shutil.rmtree(staging)


def planned_cells(tasks: list[dict], arms: list[str], budgets: list[int]) -> list[tuple]:
    cells = [(t["id"], arm, budget) for t in tasks for arm in arms for budget in budgets]
    if not cells or any(b <= 0 for b in budgets):
        raise ReuseRefused("empty matrix or nonpositive budget")
    if len(cells) != len(set(cells)):
        raise ReuseRefused("duplicate planned matrix cells")
    return cells


def matrix_coverage(planned: list[tuple], scores: list) -> dict:
    """Report unrun cells explicitly; errors/skips remain observations, not zeros."""
    expected = set(planned)
    actual, outcomes = set(), {}
    for score in scores:
        key = (score.task_id, score.arm, score.budget)
        if key not in expected or key in actual:
            raise ReuseRefused("duplicate or unplanned observed matrix cell")
        if score.outcome not in {"injected", "skipped", "error"}:
            raise ReuseRefused("unknown matrix outcome")
        actual.add(key)
        outcomes[score.outcome] = outcomes.get(score.outcome, 0) + 1
    return {"planned": len(expected), "observed": len(actual),
            "complete": actual == expected, "missing": sorted(expected - actual),
            "outcomes": outcomes}


def write_coverage(path: Path, planned: list[tuple], scores: list) -> None:
    pending = path.with_suffix(".pending")
    with pending.open("x") as f:
        json.dump(matrix_coverage(planned, scores), f, sort_keys=True)
        f.flush()
        os.fsync(f.fileno())
    os.replace(pending, path)  # exclusively this new run's bookkeeping


def check_scope(proc: Path = Path("/proc/self/cgroup"), root: Path = Path("/sys/fs/cgroup")) -> dict:
    """Require the original CPU/memory/swap envelope; caller retains wall timeout."""
    lines = [line[3:] for line in proc.read_text().splitlines() if line.startswith("0::")]
    if len(lines) != 1:
        raise ReuseRefused("unreadable cgroup-v2 scope")
    cg = root / lines[0].lstrip("/")
    limits = {key: (cg / key).read_text().strip()
              for key in ("cpu.max", "memory.max", "memory.swap.max")}
    if limits != {"cpu.max": "400000 100000", "memory.max": "6442450944",
                  "memory.swap.max": "0"}:
        raise ReuseRefused("index reuse requires original CPU400%/memory6GiB/swap0 scope")
    return limits
