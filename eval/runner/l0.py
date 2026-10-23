"""L0 offline retrieval eval (eval v2, see eval/v2/PREREGISTRATION.md).

Runs the production injection path (``bobbin hook inject-context``) on each task
prompt alone, with no agent, and scores what it would inject against the gold
hunks of the task's fixing commit. Also runs four outside baselines at the same
line budget so the ablations have something to be compared with besides each
other.

Every arm result is one of three outcomes and they are never conflated:

* ``injected`` -- the arm produced context; chunks were parsed from it.
* ``skipped``  -- the arm deliberately injected nothing (the hook said why).
* ``error``    -- the arm failed. The hook exits 0 on error by design (it must
  never block a prompt), so an empty stdout is NOT evidence of "nothing to
  inject"; the stderr line is what separates the two.
"""

from __future__ import annotations

import json
import math
import os
import random
import re
import shutil
import subprocess
import tempfile
from collections import Counter, defaultdict
from collections.abc import Callable, Iterable
from dataclasses import asdict, dataclass, field
from pathlib import Path
from typing import Any

# ---------------------------------------------------------------------------
# Preregistered constants. Changing any of these is a deviation and must be
# logged in eval/v2/PREREGISTRATION.md.

DEFAULT_BUDGET = 300
SEED = 20260924
BOOTSTRAP_RESAMPLES = 10_000

# Shipped defaults every calibration-driven arm starts from (src/config.rs).
DEFAULT_TUNING = {
    "semantic_weight": 0.9,
    "doc_demotion": 0.3,
    "rrf_k": 60.0,
    "recency_weight": 0.3,
    "bridge_mode": "inject",
}

ABLATIONS: dict[str, dict[str, Any]] = {
    "full": {},
    "-semantic": {"calibration": {"semantic_weight": 0.0}},
    "-coupling": {"index": "nocoupling"},
    "-blame": {"calibration": {"bridge_mode": "off"}},
    "-recency": {"calibration": {"recency_weight": 0.0}},
    "-docdemote": {"calibration": {"doc_demotion": 0.0}},
    "-gate": {"hook_args": ["--gate-threshold=-1"]},
}
BASELINES = ("rg", "bm25", "embed", "repomap")
ALL_ARMS = tuple(ABLATIONS) + BASELINES

# ---------------------------------------------------------------------------
# Gold


_TEST_PATH = re.compile(r"(^|/)(tests?|testing|__tests__)(/|$)")
_TEST_FILE = re.compile(
    r"(^|/)(test_[^/]*|[^/]*_tests?\.[^/]+|[^/]*\.test\.[^/]+|[^/]*\.spec\.[^/]+)$"
)
_NON_CONTEXT = re.compile(
    r"(^|/)(Cargo\.lock|poetry\.lock|package-lock\.json|yarn\.lock|pnpm-lock\.yaml|uv\.lock|go\.sum"
    r"|CHANGELOG[^/]*|CHANGES[^/]*|NEWS[^/]*)$",
    re.IGNORECASE,
)


def is_excluded_from_gold(path: str) -> bool:
    """Test files are the grader, lockfiles and changelogs are not context."""
    return bool(_TEST_PATH.search(path) or _TEST_FILE.search(path) or _NON_CONTEXT.search(path))


_HUNK = re.compile(r"^@@ -(\d+)(?:,(\d+))? \+\d+(?:,\d+)? @@")


def gold_from_diff(diff_text: str) -> dict[str, list[tuple[int, int]]]:
    """Pre-image hunk line ranges per gold file, from a ``git diff -U0``.

    A pure insertion (pre-image count 0) contributes the one pre-image line it
    follows (line 1 when inserting at the top). Files that do not exist in the
    pre-image (new files) are not gold: the agent's tree does not contain them.
    Deleted and renamed files are keyed by their pre-image path.
    """
    gold: dict[str, list[tuple[int, int]]] = {}
    current: str | None = None
    for line in diff_text.splitlines():
        if line.startswith("diff --git "):
            current = None
        elif line.startswith("--- "):
            src = line[4:].strip()
            current = None if src == "/dev/null" else src.removeprefix("a/")
            if current is not None and is_excluded_from_gold(current):
                current = None
        elif current is not None and line.startswith("@@"):
            m = _HUNK.match(line)
            if not m:
                continue
            start = int(m.group(1))
            count = 1 if m.group(2) is None else int(m.group(2))
            if count == 0:
                rng = (max(start, 1), max(start, 1))
            else:
                rng = (start, start + count - 1)
            gold.setdefault(current, []).append(rng)
    return gold


def gold_for_commit(workspace: Path, commit: str) -> dict[str, list[tuple[int, int]]]:
    diff = subprocess.run(
        ["git", "diff", "-U0", "--no-color", "--no-ext-diff", "-M", f"{commit}^", commit],
        cwd=workspace,
        check=True,
        capture_output=True,
        text=True,
    ).stdout
    return gold_from_diff(diff)


# ---------------------------------------------------------------------------
# Injected chunks

Chunk = tuple[str, int, int]  # (path, start_line, end_line), 1-based inclusive

# Standard-format per-chunk header: "--- src/a.rs:10-42 name (function, score 0.91) ---"
_CHUNK_HEADER = re.compile(r"^--- (?P<path>\S+?):(?P<s>\d+)-(?P<e>\d+)(?:\s|$)")


def parse_injected_chunks(text: str) -> list[Chunk]:
    chunks: list[Chunk] = []
    for line in text.splitlines():
        m = _CHUNK_HEADER.match(line)
        if m:
            s, e = int(m.group("s")), int(m.group("e"))
            chunks.append((m.group("path"), min(s, e), max(s, e)))
    return chunks


def classify_hook_result(returncode: int, stdout: str, stderr: str) -> tuple[str, str]:
    """(outcome, detail). See the module docstring for why stderr decides."""
    for line in stderr.splitlines():
        if line.startswith("bobbin inject-context"):
            return "error", line.strip()
    if returncode != 0:
        return "error", f"exit {returncode}: {stderr.strip()[:300]}"
    if parse_injected_chunks(stdout):
        return "injected", ""
    reasons = [ln.strip() for ln in stderr.splitlines() if ln.startswith("bobbin: skipped")]
    if reasons:
        return "skipped", reasons[0]
    if stdout.strip():
        # Output that contains no parseable chunk header is a format we do not
        # understand -- refusing to score it beats scoring it as zero.
        return "error", "injected text had no parseable chunk headers"
    return "skipped", "no output and no stated reason"


# ---------------------------------------------------------------------------
# Metrics


@dataclass
class ArmScore:
    task_id: str
    arm: str
    budget: int
    outcome: str
    detail: str = ""
    injected_lines: int = 0
    injected_tokens: int = 0
    gold_files: int = 0
    gold_hunks: int = 0
    gold_lines: int = 0
    file_recall: float | None = None
    hunk_recall: float | None = None
    file_precision: float | None = None
    density: float | None = None
    chunks: list[Chunk] = field(default_factory=list)


def _merge(ranges: Iterable[tuple[int, int]]) -> list[tuple[int, int]]:
    out: list[tuple[int, int]] = []
    for s, e in sorted(ranges):
        if out and s <= out[-1][1] + 1:
            out[-1] = (out[-1][0], max(out[-1][1], e))
        else:
            out.append((s, e))
    return out


def _overlap(a: list[tuple[int, int]], b: list[tuple[int, int]]) -> int:
    total = 0
    for s1, e1 in a:
        for s2, e2 in b:
            lo, hi = max(s1, s2), min(e1, e2)
            if lo <= hi:
                total += hi - lo + 1
    return total


def score_chunks(
    chunks: list[Chunk],
    gold: dict[str, list[tuple[int, int]]],
    injected_chars: int = 0,
) -> dict[str, Any]:
    by_file: dict[str, list[tuple[int, int]]] = defaultdict(list)
    for path, s, e in chunks:
        by_file[path].append((s, e))
    injected = {p: _merge(r) for p, r in by_file.items()}
    injected_lines = sum(e - s + 1 for r in injected.values() for s, e in r)
    gold_merged = {p: _merge(r) for p, r in gold.items()}
    gold_lines = sum(e - s + 1 for r in gold_merged.values() for s, e in r)
    gold_hunks = [(p, rng) for p, rs in gold.items() for rng in rs]

    hit_files = [p for p in gold if p in injected]
    hit_hunks = [1 for p, rng in gold_hunks if p in injected and _overlap([rng], injected[p]) > 0]
    gold_lines_injected = sum(
        _overlap(gold_merged[p], injected[p]) for p in gold_merged if p in injected
    )

    return {
        "injected_lines": injected_lines,
        "injected_tokens": injected_chars // 4,
        "gold_files": len(gold),
        "gold_hunks": len(gold_hunks),
        "gold_lines": gold_lines,
        "file_recall": len(hit_files) / len(gold) if gold else None,
        "hunk_recall": len(hit_hunks) / len(gold_hunks) if gold_hunks else None,
        "file_precision": (sum(1 for p in injected if p in gold) / len(injected))
        if injected
        else None,
        # Undefined (not 0) when nothing was injected: see PREREGISTRATION "density".
        "density": (gold_lines_injected / injected_lines) if injected_lines else None,
    }


# ---------------------------------------------------------------------------
# Isolation


def isolated_env(scratch: Path) -> dict[str, str]:
    """Environment for every bobbin call: no remote server, no host config.

    The hook resolves a server from --server / BOBBIN_SERVER / repo config /
    GLOBAL config. A host whose global config names a shared server would
    otherwise route L0 to it without a word.
    """
    env = {k: v for k, v in os.environ.items() if not k.startswith("BOBBIN_")}
    cfg = scratch / "xdg-config"
    cfg.mkdir(parents=True, exist_ok=True)
    env["XDG_CONFIG_HOME"] = str(cfg)
    return env


# ---------------------------------------------------------------------------
# Ablation arms (the production hook)


def write_calibration(workspace: Path, overrides: dict[str, Any]) -> None:
    """Every hook arm writes a complete calibration file (see PREREGISTRATION)."""
    tuning = {**DEFAULT_TUNING, **overrides}
    calibration = {
        "calibrated_at": "eval-v2-l0",
        "snapshot": {
            "chunk_count": 0,
            "file_count": 0,
            "primary_language": "",
            "language_distribution": [],
            "repo_age_days": 0,
            "recent_commit_rate": 0.0,
        },
        "best_config": {
            "semantic_weight": tuning["semantic_weight"],
            "doc_demotion": tuning["doc_demotion"],
            "rrf_k": tuning["rrf_k"],
            "recency_weight": tuning["recency_weight"],
            "bridge_mode": tuning["bridge_mode"],
        },
        "top_results": [],
        "sample_count": 0,
        "probe_count": 0,
        "terse_warning": False,
    }
    data = workspace / ".bobbin"
    data.mkdir(exist_ok=True)
    (data / "calibration.json").write_text(json.dumps(calibration, indent=2))


def run_hook_arm(
    workspace: Path,
    prompt: str,
    arm: str,
    budget: int,
    env: dict[str, str],
    bobbin: str,
    *,
    timeout: int = 180,
) -> tuple[str, str, list[Chunk], int]:
    spec = ABLATIONS[arm]
    write_calibration(workspace, spec.get("calibration", {}))
    cmd = [
        bobbin,
        "hook",
        "inject-context",
        "--no-dedup",
        "--format-mode",
        "standard",
        "--budget",
        str(budget),
        *spec.get("hook_args", []),
    ]
    payload = json.dumps(
        {"prompt": prompt, "cwd": str(workspace), "session_id": f"eval-v2-l0-{arm}"}
    )
    try:
        r = subprocess.run(
            cmd,
            cwd=workspace,
            input=payload,
            capture_output=True,
            text=True,
            timeout=timeout,
            env=env,
            check=False,
        )
    except subprocess.TimeoutExpired:
        return "error", f"timeout after {timeout}s", [], 0
    outcome, detail = classify_hook_result(r.returncode, r.stdout, r.stderr)
    chunks = parse_injected_chunks(r.stdout) if outcome == "injected" else []
    return outcome, detail, chunks, len(r.stdout)


# ---------------------------------------------------------------------------
# Baselines

_STOP = frozenset(
    "the a an and or of to in on for with without from into when that this these those is are was "
    "were be been being it its as at by not no so if then else also only should must can could "
    "would will make makes made use used using fix fixes fixed add adds added remove removes "
    "removed change changes changed update updates updated instead which where while after before "
    "all any each other some such than more most less same new old currently previously now".split()
)
_WORD = re.compile(r"[A-Za-z_][A-Za-z0-9_]{2,}")


def prompt_terms(prompt: str) -> list[str]:
    seen: dict[str, None] = {}
    for w in _WORD.findall(prompt):
        if w.lower() not in _STOP:
            seen.setdefault(w, None)
    return list(seen)


def tracked_text_files(workspace: Path, max_bytes: int = 1_000_000) -> list[str]:
    out = subprocess.run(
        ["git", "ls-files", "-z"], cwd=workspace, check=True, capture_output=True
    ).stdout.split(b"\0")
    files = []
    for raw in out:
        if not raw:
            continue
        rel = raw.decode("utf-8", "replace")
        p = workspace / rel
        try:
            if p.is_file() and p.stat().st_size <= max_bytes:
                with p.open("rb") as fh:
                    if b"\0" not in fh.read(4096):
                        files.append(rel)
        except OSError:
            continue
    return files


def _fill_budget(candidates: Iterable[Chunk], budget: int) -> list[Chunk]:
    """Take chunks in order until the line budget is spent (last one truncated)."""
    taken: list[Chunk] = []
    used = 0
    for path, s, e in candidates:
        if used >= budget:
            break
        n = e - s + 1
        if used + n > budget:
            e = s + (budget - used) - 1
            n = e - s + 1
        taken.append((path, s, e))
        used += n
    return taken


def _line_counts(workspace: Path, files: Iterable[str]) -> dict[str, int]:
    counts = {}
    for f in files:
        try:
            with (workspace / f).open("rb") as fh:
                counts[f] = sum(1 for _ in fh)
        except OSError:
            counts[f] = 0
    return counts


def baseline_rg(workspace: Path, prompt: str, budget: int, *, window: int = 10) -> list[Chunk]:
    terms = prompt_terms(prompt)
    if not terms or not shutil.which("rg"):
        return []
    args = [
        "rg",
        "--no-heading",
        "--line-number",
        "--no-messages",
        "-w",
        "-F",
        "--glob",
        "!.bobbin",
    ]
    for t in terms:
        args += ["-e", t]
    r = subprocess.run(args, cwd=workspace, capture_output=True, text=True, check=False)
    hits: dict[str, list[int]] = defaultdict(list)
    for line in r.stdout.splitlines():
        parts = line.split(":", 2)
        if len(parts) >= 2 and parts[1].isdigit():
            hits[parts[0]].append(int(parts[1]))
    ranked = sorted(hits, key=lambda f: (-len(hits[f]), f))
    lengths = _line_counts(workspace, ranked)
    cands: list[Chunk] = []
    for f in ranked:
        for s, e in _merge(
            (max(1, n - window), min(lengths.get(f, n + window) or n, n + window)) for n in hits[f]
        ):
            cands.append((f, s, e))
    return _fill_budget(cands, budget)


_TOKEN = re.compile(r"[A-Za-z0-9]+")


def _tokens(text: str) -> list[str]:
    out = []
    for tok in _TOKEN.findall(text):
        # split camelCase too, keep the whole token as well
        parts = re.findall(r"[A-Z]?[a-z0-9]+|[A-Z]+(?![a-z])", tok)
        out.append(tok.lower())
        out.extend(p.lower() for p in parts if p.lower() != tok.lower())
    return out


def baseline_bm25(
    workspace: Path, prompt: str, budget: int, *, window: int = 40, k1: float = 1.5, b: float = 0.75
) -> list[Chunk]:
    query = [t for t in _tokens(prompt) if t not in _STOP and len(t) > 2]
    if not query:
        return []
    docs: list[tuple[Chunk, Counter]] = []
    for f in tracked_text_files(workspace):
        try:
            lines = (workspace / f).read_text(errors="replace").splitlines()
        except OSError:
            continue
        for i in range(0, max(len(lines), 1), window):
            seg = lines[i : i + window]
            if not seg:
                continue
            docs.append(((f, i + 1, i + len(seg)), Counter(_tokens("\n".join(seg)))))
    if not docs:
        return []
    n = len(docs)
    avgdl = sum(sum(c.values()) for _, c in docs) / n
    df = Counter()
    for _, c in docs:
        for t in set(query):
            if t in c:
                df[t] += 1
    idf = {t: math.log(1 + (n - df[t] + 0.5) / (df[t] + 0.5)) for t in set(query)}
    scored = []
    for chunk, c in docs:
        dl = sum(c.values())
        s = 0.0
        for t in query:
            tf = c.get(t, 0)
            if tf:
                s += idf[t] * tf * (k1 + 1) / (tf + k1 * (1 - b + b * dl / avgdl))
        if s > 0:
            scored.append((s, chunk))
    scored.sort(key=lambda x: (-x[0], x[1]))
    return _fill_budget((c for _, c in scored), budget)


def baseline_embed(
    workspace: Path, prompt: str, budget: int, env: dict[str, str], bobbin: str, *, limit: int = 60
) -> list[Chunk]:
    r = subprocess.run(
        [bobbin, "search", "--json", "--mode", "semantic", "--limit", str(limit), prompt],
        cwd=workspace,
        capture_output=True,
        text=True,
        env=env,
        timeout=180,
        check=False,
    )
    if r.returncode != 0:
        raise RuntimeError(f"bobbin search failed: {r.stderr.strip()[:300]}")
    results = json.loads(r.stdout).get("results", [])
    cands = [
        (x["file_path"], int(x["start_line"]), int(x["end_line"]))
        for x in results
        if x.get("source", "code") == "code"
    ]
    return _fill_budget(cands, budget)


def baseline_repomap(
    workspace: Path,
    prompt: str,
    budget: int,
    env: dict[str, str],
    bobbin: str,
    *,
    max_files: int = 40,
) -> list[Chunk]:
    """Repo-map-STYLE outline (not Aider's PageRank map; see PREREGISTRATION).

    Files ranked by prompt-term overlap (path + content hits), then each file's
    parsed symbol signatures, one line per symbol, until the budget.
    """
    terms = [t.lower() for t in prompt_terms(prompt)]
    if not terms:
        return []
    scores: list[tuple[int, str]] = []
    for f in tracked_text_files(workspace):
        try:
            text = (workspace / f).read_text(errors="replace").lower()
        except OSError:
            continue
        s = sum(text.count(t) for t in terms) + 5 * sum(t in f.lower() for t in terms)
        if s:
            scores.append((s, f))
    scores.sort(key=lambda x: (-x[0], x[1]))
    cands: list[Chunk] = []
    for _, f in scores[:max_files]:
        r = subprocess.run(
            [bobbin, "refs", "symbols", "--json", f],
            cwd=workspace,
            capture_output=True,
            text=True,
            env=env,
            timeout=60,
            check=False,
        )
        if r.returncode != 0:
            continue
        try:
            symbols = json.loads(r.stdout).get("symbols", [])
        except json.JSONDecodeError:
            continue
        for sym in symbols:
            ln = int(sym["start_line"])
            cands.append((f, ln, ln))
    return _fill_budget(cands, budget)


# ---------------------------------------------------------------------------
# Paired statistics (preregistered: bootstrap CI, Wilcoxon signed-rank, Holm)


def paired_bootstrap_ci(
    diffs: list[float], *, resamples: int = BOOTSTRAP_RESAMPLES, seed: int = SEED
) -> tuple[float, float]:
    if not diffs:
        return (float("nan"), float("nan"))
    rng = random.Random(seed)
    n = len(diffs)
    means = sorted(sum(diffs[rng.randrange(n)] for _ in range(n)) / n for _ in range(resamples))
    return means[int(0.025 * resamples)], means[int(0.975 * resamples) - 1]


def wilcoxon_signed_rank(diffs: list[float]) -> float:
    """Two-sided p-value, normal approximation with tie and zero handling (Pratt dropped zeros)."""
    nz = [d for d in diffs if d != 0]
    n = len(nz)
    if n == 0:
        return 1.0
    order = sorted(range(n), key=lambda i: abs(nz[i]))
    ranks = [0.0] * n
    i = 0
    tie_term = 0.0
    while i < n:
        j = i
        while j + 1 < n and abs(nz[order[j + 1]]) == abs(nz[order[i]]):
            j += 1
        avg = (i + j) / 2 + 1
        for k in range(i, j + 1):
            ranks[order[k]] = avg
        t = j - i + 1
        tie_term += t**3 - t
        i = j + 1
    w_plus = sum(r for r, d in zip(ranks, nz) if d > 0)
    mean = n * (n + 1) / 4
    var = n * (n + 1) * (2 * n + 1) / 24 - tie_term / 48
    if var <= 0:
        return 1.0
    z = (abs(w_plus - mean) - 0.5) / math.sqrt(var)
    return min(1.0, math.erfc(max(z, 0.0) / math.sqrt(2)))


def holm(pvalues: dict[str, float]) -> dict[str, float]:
    items = sorted(pvalues.items(), key=lambda kv: kv[1])
    m = len(items)
    adjusted: dict[str, float] = {}
    running = 0.0
    for i, (k, p) in enumerate(items):
        running = max(running, min(1.0, (m - i) * p))
        adjusted[k] = running
    return adjusted


def compare_to_reference(
    scores: list[ArmScore], metric: str, *, reference: str = "full", budget: int = DEFAULT_BUDGET
) -> dict[str, dict[str, Any]]:
    """Paired by task, tasks where both arms define the metric. Holm across arms."""
    table: dict[tuple[str, str], float] = {}
    for s in scores:
        v = getattr(s, metric)
        if s.budget == budget and s.outcome != "error" and v is not None:
            table[(s.task_id, s.arm)] = v
    arms = sorted({a for _, a in table} - {reference})
    rows: dict[str, dict[str, Any]] = {}
    raw_p: dict[str, float] = {}
    for arm in arms:
        diffs = [
            table[(t, arm)] - table[(t, reference)]
            for (t, a) in table
            if a == arm and (t, reference) in table
        ]
        lo, hi = paired_bootstrap_ci(diffs)
        p = wilcoxon_signed_rank(diffs)
        raw_p[arm] = p
        rows[arm] = {
            "n": len(diffs),
            "mean_diff": sum(diffs) / len(diffs) if diffs else float("nan"),
            "ci95": [lo, hi],
            "p": p,
        }
    for arm, p_adj in holm(raw_p).items():
        rows[arm]["p_holm"] = p_adj
    return rows


# ---------------------------------------------------------------------------
# Driver


def run_task(
    task: dict[str, Any],
    workspace: Path,
    arms: Iterable[str],
    budgets: Iterable[int],
    bobbin: str,
    env: dict[str, str],
    index_workspace: Callable[[Path, dict[str, str] | None], None],
    *,
    nocoupling_workspace: Path | None = None,
    normalize_paths: bool = False,
) -> list[ArmScore]:
    """Score every (arm, budget) for one task on an already-checked-out workspace.

    ``workspace`` is at ``commit^`` with a default index. ``index_workspace`` is
    called to build the no-coupling index in a sibling copy when that arm is
    requested.
    """
    gold = gold_for_commit(workspace, task["commit"])
    prompt = task["description"].strip()
    results: list[ArmScore] = []
    arms = list(arms)
    nocoupling_ws: Path | None = None
    if "-coupling" in arms:
        nocoupling_ws = nocoupling_workspace or workspace.parent / (workspace.name + "-nocoupling")
        if not nocoupling_ws.exists():
            shutil.copytree(
                workspace, nocoupling_ws, symlinks=True, ignore=shutil.ignore_patterns(".bobbin")
            )
        index_workspace(nocoupling_ws, {"git.coupling_enabled": "false"})
    for budget in budgets:
        for arm in arms:
            base = ArmScore(task_id=task["id"], arm=arm, budget=budget, outcome="error")
            if not gold:
                base.outcome, base.detail = "error", "no gold files after exclusion"
                results.append(base)
                continue
            try:
                chars = 0
                scoring_workspace = workspace
                if arm in ABLATIONS:
                    ws = nocoupling_ws if arm == "-coupling" else workspace
                    scoring_workspace = ws
                    outcome, detail, chunks, chars = run_hook_arm(
                        ws, prompt, arm, budget, env, bobbin
                    )
                else:
                    if arm == "rg":
                        chunks = baseline_rg(workspace, prompt, budget)
                    elif arm == "bm25":
                        chunks = baseline_bm25(workspace, prompt, budget)
                    elif arm == "embed":
                        chunks = baseline_embed(workspace, prompt, budget, env, bobbin)
                    elif arm == "repomap":
                        chunks = baseline_repomap(workspace, prompt, budget, env, bobbin)
                    else:
                        raise ValueError(f"unknown arm {arm}")
                    outcome, detail = (
                        ("injected", "") if chunks else ("skipped", "baseline found nothing")
                    )
                if normalize_paths:
                    from runner.score_paths import workspace_chunks
                    chunks = workspace_chunks(scoring_workspace, chunks)
                if arm not in ABLATIONS:
                    chars = _chunk_chars(workspace, chunks, strict=normalize_paths)
            except Exception as exc:  # noqa: BLE001 -- recorded, never swallowed
                base.outcome, base.detail = "error", f"{type(exc).__name__}: {exc}"[:300]
                results.append(base)
                continue
            m = score_chunks(chunks, gold, chars)
            results.append(
                ArmScore(
                    task_id=task["id"],
                    arm=arm,
                    budget=budget,
                    outcome=outcome,
                    detail=detail,
                    chunks=chunks,
                    **m,
                )
            )
    return results


def _chunk_chars(workspace: Path, chunks: list[Chunk], *, strict: bool = False) -> int:
    total = 0
    for path, s, e in chunks:
        try:
            lines = (workspace / path).read_text(errors="replace").splitlines()
        except OSError as exc:
            if strict:
                raise ValueError("unreadable native chunk path") from exc
            continue
        total += sum(len(x) + 1 for x in lines[s - 1 : e])
    return total


def write_jsonl(path: Path, scores: Iterable[ArmScore]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("a") as fh:
        for s in scores:
            fh.write(json.dumps(asdict(s)) + "\n")


def read_jsonl(path: Path) -> list[ArmScore]:
    out = []
    for line in path.read_text().splitlines():
        if line.strip():
            d = json.loads(line)
            if d.get("kind") == "manifest":
                continue
            d["chunks"] = [tuple(c) for c in d.get("chunks", [])]
            out.append(ArmScore(**d))
    return out


def summarize(scores: list[ArmScore], budget: int = DEFAULT_BUDGET) -> dict[str, Any]:
    outcomes: dict[str, Counter] = defaultdict(Counter)
    for s in scores:
        if s.budget == budget:
            outcomes[s.arm][s.outcome] += 1
    return {
        "budget": budget,
        "outcomes": {a: dict(c) for a, c in sorted(outcomes.items())},
        "hunk_recall": compare_to_reference(scores, "hunk_recall", budget=budget),
        "density": compare_to_reference(scores, "density", budget=budget),
    }


def new_scratch() -> Path:
    return Path(tempfile.mkdtemp(prefix="bobbin-eval-l0-"))
