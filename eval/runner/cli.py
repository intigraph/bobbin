"""CLI entrypoint for the bobbin eval runner.

Provides commands for running evaluations, scoring results, and generating
reports. The runner orchestrates workspace setup, agent invocation, and
scoring for each task × approach × attempt combination.

Usage::

    bobbin-eval run-task ruff-001
    bobbin-eval run-all --tasks-dir tasks
    bobbin-eval score results/
    bobbin-eval report results/ --output report.md
"""

from __future__ import annotations

import json
import logging
import os
import secrets
import shutil
import sys
import tempfile
from datetime import datetime, timezone
from pathlib import Path

import click

from runner.bobbin_setup import (
    ALLOWED_OVERRIDE_KEYS,
    generate_override_settings,
    parse_config_override,
)
from runner.task_loader import TaskLoadError, load_all_tasks, load_task_by_id
from scorer.attribution import (
    ServingModelMismatch,
    check_comparable,
    partition_by_attribution,
    serving_model_counts,
    serving_model_from_usage,
)

logger = logging.getLogger(__name__)

_EVAL_ROOT = Path(__file__).resolve().parent.parent
_DEFAULT_SETTINGS = _EVAL_ROOT / "settings-with-bobbin.json"
_NO_BOBBIN_SETTINGS = _EVAL_ROOT / "settings-no-bobbin.json"


def _parse_override_specs(specs: tuple[str, ...]) -> list[dict[str, str]]:
    """Parse ``--config-overrides`` specs into per-ablation override dicts.

    Each spec is a ``key=value`` string.  Each spec becomes a separate
    ablation variant (one override at a time) to isolate the effect of
    individual parameters.

    Returns a list of single-key dicts, one per ablation variant.
    """
    variants: list[dict[str, str]] = []
    for spec in specs:
        key, raw_value = parse_config_override(spec)
        variants.append({key: raw_value})
    return variants


def _override_approach_name(overrides: dict[str, str]) -> str:
    """Build a short approach name for an override variant.

    Example: ``"with-bobbin+semantic_weight=0.0"``
    """
    parts = [f"{k}={v}" for k, v in sorted(overrides.items())]
    return "with-bobbin+" + ",".join(parts)


def _setup_logging(verbose: bool) -> None:
    """Configure logging based on verbosity."""
    level = logging.DEBUG if verbose else logging.INFO
    logging.basicConfig(
        level=level,
        format="%(asctime)s %(levelname)s %(name)s: %(message)s",
        datefmt="%H:%M:%S",
    )


def _check_session_budget(force: bool = False) -> None:
    """Check Claude account session budget before starting expensive eval runs.

    Reads ~/.claude/.credentials.json for token expiry and active session count.
    Aborts if token is expired or critical (< 1h remaining).
    Warns if token is low (< 4h remaining).

    Pass force=True to skip the check (--force-budget flag).
    """
    if force:
        return

    creds_path = Path.home() / ".claude" / ".credentials.json"
    if not creds_path.exists():
        click.echo("Warning: No Claude credentials found, cannot check session budget.", err=True)
        return

    try:
        creds = json.loads(creds_path.read_text())
        oauth = creds.get("claudeAiOauth", {})
        expires_ms = oauth.get("expiresAt")
        subscription = oauth.get("subscriptionType", "unknown")
        tier = oauth.get("rateLimitTier", "unknown")

        if not expires_ms:
            click.echo("Warning: Could not read token expiry from credentials.", err=True)
            return

        now_ms = datetime.now(timezone.utc).timestamp() * 1000
        remaining_h = (expires_ms - now_ms) / 3_600_000

        if remaining_h <= 0:
            click.echo(
                f"ABORT: Claude token has EXPIRED. "
                f"Subscription: {subscription}, tier: {tier}. "
                f"Run `ccm` to refresh or switch accounts.",
                err=True,
            )
            sys.exit(2)

        if remaining_h < 1:
            click.echo(
                f"ABORT: Claude token expires in {remaining_h:.1f}h — too low for eval runs. "
                f"Use --force-budget to override.",
                err=True,
            )
            sys.exit(2)

        if remaining_h < 4:
            click.echo(
                f"WARNING: Claude token expires in {remaining_h:.1f}h. "
                f"Eval runs may be interrupted by token expiry.",
                err=True,
            )

        # Check active session count (proxy for load)
        try:
            import subprocess
            result = subprocess.run(
                ["tmux", "list-sessions", "-F", "#{session_name}"],
                capture_output=True, text=True, timeout=5,
            )
            sessions = [s for s in result.stdout.splitlines() if s.startswith("gt-")]
            if len(sessions) > 10:
                click.echo(
                    f"WARNING: {len(sessions)} active Gas Town sessions. "
                    f"Eval runs will compete for rate limit budget.",
                    err=True,
                )
        except Exception:
            pass  # tmux check is best-effort

        click.echo(
            f"Session budget: {subscription} ({tier}), "
            f"{remaining_h:.1f}h remaining",
            err=True,
        )

    except (json.JSONDecodeError, KeyError, OSError) as exc:
        click.echo(f"Warning: Could not check session budget: {exc}", err=True)


def _resolve_tasks_dir(tasks_dir: str) -> Path:
    """Resolve tasks directory relative to the eval root."""
    p = Path(tasks_dir)
    if p.is_absolute():
        return p
    # Try relative to cwd first, then relative to eval/ directory.
    if p.is_dir():
        return p
    eval_root = Path(__file__).parent.parent
    candidate = eval_root / p
    if candidate.is_dir():
        return candidate
    return p  # Let downstream raise the error.


def _build_prompt(task: dict, approach: str = "no-bobbin") -> str:
    """Build the agent prompt from a task definition."""
    repo = task["repo"]
    desc = task["description"].strip()
    test_cmd = task["test_command"]
    base = (
        f"You are working on the {repo} project.\n\n"
        f"{desc}\n\n"
        f"Implement the fix. Run the test suite with `{test_cmd}` to verify."
    )
    # Override approaches (with-bobbin+key=value) also get the bobbin prompt.
    if approach == "with-bobbin" or approach.startswith("with-bobbin+"):
        base += (
            "\n\nThis project has bobbin installed (a semantic code search engine). "
            "Before exploring manually, use bobbin to find relevant code:\n"
            "- `bobbin search \"<key terms from the task>\"` — semantic + keyword search\n"
            "- `bobbin related <file>` — find test files and co-changing dependencies\n"
            "- `bobbin refs <SymbolName>` — trace definitions and usages\n"
            "Start with bobbin search to orient yourself, then read the files it identifies."
        )
    return base


def _extract_token_usage(result: dict | None) -> dict | None:
    """Extract token usage and cost info from the agent's JSON result."""
    if not result or not isinstance(result, dict):
        return None
    usage = result.get("usage", {})
    return {
        "total_cost_usd": result.get("total_cost_usd"),
        "input_tokens": usage.get("input_tokens", 0),
        "output_tokens": usage.get("output_tokens", 0),
        "cache_creation_tokens": usage.get("cache_creation_input_tokens", 0),
        "cache_read_tokens": usage.get("cache_read_input_tokens", 0),
        "num_turns": result.get("num_turns"),
        "model_usage": result.get("modelUsage"),
    }


def _generate_run_id() -> str:
    """Generate a unique run ID in ``YYYYMMDD-HHMMSS-XXXX`` format.

    Uses UTC time and 4 random hex characters for uniqueness.
    """
    now = datetime.now(timezone.utc)
    return f"{now.strftime('%Y%m%d-%H%M%S')}-{secrets.token_hex(2)}"


def _save_result(result: dict, results_dir: Path, *, run_id: str | None = None) -> Path:
    """Save a single run result as a JSON file.

    Filename format: ``<task_id>_<approach>_<attempt>.json``

    When *run_id* is provided the file is saved under
    ``results/runs/<run_id>/`` and the ``run_id`` field is added to the
    result dict.  Otherwise the legacy flat layout is used.
    """
    if run_id is not None:
        result["run_id"] = run_id
        target_dir = results_dir / "runs" / run_id
    else:
        target_dir = results_dir

    target_dir.mkdir(parents=True, exist_ok=True)
    task_id = result.get("task_id", "unknown")
    approach = result.get("approach", "unknown")
    attempt = result.get("attempt", 0)
    filename = f"{task_id}_{approach}_{attempt}.json"
    path = target_dir / filename
    path.write_text(json.dumps(result, indent=2, default=str), encoding="utf-8")
    return path


def _save_raw_stream(
    output_raw: str,
    results_dir: Path,
    *,
    task_id: str,
    approach: str,
    attempt: int,
    run_id: str | None = None,
) -> Path | None:
    """Save the raw JSONL stream output alongside the result JSON.

    Writes ``<task_id>_<approach>_<attempt>.stream.jsonl`` into the same
    directory as the result file.  Best-effort: logs warnings on failure
    but never raises.
    """
    try:
        if not output_raw:
            return None

        if run_id is not None:
            target_dir = results_dir / "runs" / run_id
        else:
            target_dir = results_dir
        target_dir.mkdir(parents=True, exist_ok=True)

        filename = f"{task_id}_{approach}_{attempt}.stream.jsonl"
        dest = target_dir / filename
        dest.write_text(output_raw, encoding="utf-8")
        return dest
    except Exception as exc:
        logger.warning("Failed to save raw stream: %s", exc)
        return None


def _preserve_raw_metrics(
    ws: Path,
    results_dir: Path,
    *,
    task_id: str,
    approach: str,
    attempt: int,
    run_id: str | None = None,
) -> Path | None:
    """Copy raw metrics.jsonl from workspace to results directory.

    Returns the destination path if the file was copied, ``None`` otherwise.
    The file is saved as ``<task_id>_<approach>_<attempt>_metrics.jsonl``
    alongside the result JSON.
    """
    raw_metrics_src = ws / ".bobbin" / "metrics.jsonl"
    if not raw_metrics_src.exists():
        return None

    if run_id is not None:
        target_dir = results_dir / "runs" / run_id
    else:
        target_dir = results_dir
    target_dir.mkdir(parents=True, exist_ok=True)

    filename = f"{task_id}_{approach}_{attempt}_metrics.jsonl"
    dest = target_dir / filename
    shutil.copy2(raw_metrics_src, dest)
    return dest


def _write_manifest(
    results_dir: Path,
    run_id: str,
    *,
    started_at: str,
    completed_at: str,
    model: str,
    budget: float,
    timeout: int,
    index_timeout: int,
    attempts_per_approach: int,
    approaches: list[str],
    tasks: list[str],
    total_results: int,
    completed_results: int,
) -> Path:
    """Write ``manifest.json`` into the run directory."""
    run_dir = results_dir / "runs" / run_id
    run_dir.mkdir(parents=True, exist_ok=True)
    manifest = {
        "run_id": run_id,
        "started_at": started_at,
        "completed_at": completed_at,
        "agent_config": {
            "model": model,
            "budget_usd": budget,
            "timeout_seconds": timeout,
            "index_timeout_seconds": index_timeout,
        },
        "attempts_per_approach": attempts_per_approach,
        "approaches": approaches,
        "tasks": tasks,
        "total_results": total_results,
        "completed_results": completed_results,
    }
    path = run_dir / "manifest.json"
    path.write_text(json.dumps(manifest, indent=2), encoding="utf-8")
    return path


def _run_single(
    task: dict,
    approach: str,
    attempt: int,
    results_dir: Path,
    *,
    run_id: str | None = None,
    settings_file: str | None = None,
    model: str = "claude-opus-4-6",
    budget: float = 100.00,
    timeout: int = 3600,
    index_timeout: int = 600,
    skip_verify: bool = False,
    save_stream: bool = True,
    config_overrides: dict[str, str] | None = None,
) -> dict:
    """Execute a single task × approach × attempt evaluation run.

    Returns the result dict (also saved to results_dir).
    """
    from runner.agent_runner import run_agent
    from runner.bobbin_setup import setup_bobbin
    from runner.workspace import collect_loc_stats, diff_snapshot, setup_workspace, snapshot
    from scorer.diff_scorer import score_diff
    from scorer.injection_scorer import score_injection_usage
    from scorer.test_scorer import run_tests

    # Resolve settings file to absolute path so it works from any cwd.
    if settings_file:
        settings_file = str(Path(settings_file).resolve())

    task_id = task["id"]
    click.echo(f"  [{task_id}] {approach} attempt {attempt + 1}")

    # 1. Setup workspace.
    with tempfile.TemporaryDirectory(prefix=f"bobbin-eval-{task_id}-") as tmpdir:
        click.echo("    Setting up workspace...")
        try:
            ws, parent = setup_workspace(
                task["repo"],
                task["commit"],
                task["test_command"],
                tmpdir,
                setup_command=task.get("setup_command"),
                verify=not skip_verify,
                setup_timeout=900,
            )
        except Exception as exc:
            click.echo(f"    Workspace setup failed: {exc}", err=True)
            result = {
                "task_id": task_id,
                "approach": approach,
                "attempt": attempt,
                "status": "workspace_error",
                "serving_model": None,
                "error": str(exc),
                "timestamp": datetime.now(timezone.utc).isoformat(),
            }
            _save_result(result, results_dir, run_id=run_id)
            return result

        # 2a. Collect LOC stats via tokei.
        project_metadata = None
        try:
            project_metadata = collect_loc_stats(ws)
        except Exception as exc:
            click.echo(f"    LOC stats collection failed (non-fatal): {exc}", err=True)

        # 2b. Bobbin setup (with-bobbin and override approaches).
        pre_agent_baseline = None
        bobbin_metadata = None
        is_bobbin_approach = approach == "with-bobbin" or approach.startswith("with-bobbin+")
        if is_bobbin_approach:
            override_label = ""
            if config_overrides:
                override_label = f" (overrides: {config_overrides})"
            click.echo(f"    Running bobbin init + index...{override_label}")
            try:
                bobbin_metadata = setup_bobbin(
                    str(ws),
                    timeout=index_timeout,
                    config_overrides=config_overrides,
                )
            except Exception as exc:
                click.echo(f"    Bobbin setup failed: {exc}", err=True)
                result = {
                    "task_id": task_id,
                    "approach": approach,
                    "attempt": attempt,
                    "status": "bobbin_setup_error",
                    "serving_model": None,
                    "error": str(exc),
                    "timestamp": datetime.now(timezone.utc).isoformat(),
                    "config_overrides": config_overrides,
                }
                _save_result(result, results_dir, run_id=run_id)
                return result
            # Snapshot after bobbin setup so diff scoring excludes bobbin
            # infrastructure files (.bobbin/, .gitignore changes).
            pre_agent_baseline = snapshot(ws)

        # 3. Run agent.
        prompt = _build_prompt(task, approach=approach)
        click.echo(f"    Running Claude Code (model={model}, budget=${budget:.2f})...")
        # Always pass a settings file to isolate from user's global hooks.
        # no-bobbin gets a clean settings file; with-bobbin gets the bobbin one.
        # Override approaches get a modified settings file with hook-level overrides.
        if is_bobbin_approach and config_overrides and settings_file:
            override_settings_path = str(
                Path(tmpdir) / f"settings-override-{attempt}.json"
            )
            run_settings = generate_override_settings(
                settings_file, config_overrides, override_settings_path,
            )
        elif is_bobbin_approach:
            run_settings = settings_file
        else:
            run_settings = str(_NO_BOBBIN_SETTINGS) if _NO_BOBBIN_SETTINGS.exists() else None

        # Set metrics source so bobbin tags all events for this run.
        metrics_tag = f"{task_id}_{approach}_{attempt}"
        os.environ["BOBBIN_METRICS_SOURCE"] = metrics_tag
        agent_result = run_agent(
            str(ws),
            prompt,
            settings_file=run_settings,
            model=model,
            max_budget_usd=budget,
            timeout=timeout,
        )
        os.environ.pop("BOBBIN_METRICS_SOURCE", None)

        # 4. Snapshot and score.
        click.echo("    Scoring...")
        snap = snapshot(ws)
        test_result = run_tests(str(ws), task["test_command"])
        diff_result = score_diff(
            str(ws), task["commit"],
            snapshot=snap,
            baseline=pre_agent_baseline,
        )

        # Capture diffs for post-hoc LLM judge comparison.
        diff_base = pre_agent_baseline or parent
        agent_diff = diff_snapshot(ws, diff_base, snap)
        ground_truth_diff = diff_snapshot(ws, parent, task["commit"])

        # 5. Read bobbin metrics (bobbin approaches only).
        bobbin_metrics = None
        if is_bobbin_approach:
            bobbin_metrics = _read_bobbin_metrics(
                ws, metrics_tag, diff_result.get("ground_truth_files", []),
            )

        # 5b. Score injection usage (with-bobbin only).
        injection_result = None
        if bobbin_metrics and bobbin_metrics.get("injected_files"):
            injection_result = score_injection_usage(
                injected_files=bobbin_metrics["injected_files"],
                files_touched=diff_result.get("files_touched", []),
            )

        # 6. Preserve raw metrics.jsonl before temp workspace cleanup.
        _preserve_raw_metrics(
            ws, results_dir,
            task_id=task_id, approach=approach, attempt=attempt,
            run_id=run_id,
        )

        # 6b. Save raw stream output alongside results.
        if save_stream:
            _save_raw_stream(
                agent_result.get("output_raw", ""),
                results_dir,
                task_id=task_id, approach=approach, attempt=attempt,
                run_id=run_id,
            )

        # Serving-model attribution (bobbin-daa): recorded from the agent's
        # own usage record, never from the model the config requested — a
        # declared-intent field is what allowed the model-mixed study in
        # docs/plans/paper-statistics.md §4b.  No usage record means an
        # explicit null: the run is unattributed, not assumed.
        agent_metrics = _extract_cost_metrics(agent_result)
        serving_model = serving_model_from_usage(agent_metrics.get("model_usage"))

        result = {
            "task_id": task_id,
            "approach": approach,
            "attempt": attempt,
            "status": "completed",
            "serving_model": serving_model,
            "timestamp": datetime.now(timezone.utc).isoformat(),
            "task": {
                "repo": task["repo"],
                "commit": task["commit"],
                "test_command": task["test_command"],
                "language": task.get("language"),
                "difficulty": task.get("difficulty"),
            },
            "agent_config": {
                "model": model,
                "budget_usd": budget,
                "timeout_seconds": timeout,
                "index_timeout_seconds": index_timeout,
            },
            "agent_result": {
                "exit_code": agent_result["exit_code"],
                "duration_seconds": agent_result["duration_seconds"],
                "timed_out": agent_result["timed_out"],
                **agent_metrics,
            },
            "token_usage": _extract_token_usage(agent_result.get("result")),
            "agent_output": {
                "num_turns": (agent_result.get("result") or {}).get("num_turns"),
                "session_id": (agent_result.get("result") or {}).get("session_id"),
                "stop_reason": (agent_result.get("result") or {}).get("stop_reason"),
            },
            "agent_stderr": (agent_result.get("stderr") or "")[:5000],
            "test_result": {
                "passed": test_result["passed"],
                "total": test_result["total"],
                "failures": test_result["failures"],
                "parsed": test_result["parsed"],
                "output": (test_result["output"] or "")[:50_000],
                "exit_code": test_result["exit_code"],
                "timed_out": test_result["timed_out"],
            },
            "diff_result": {
                "file_precision": diff_result["file_precision"],
                "file_recall": diff_result["file_recall"],
                "f1": diff_result["f1"],
                "files_touched": diff_result["files_touched"],
                "ground_truth_files": diff_result["ground_truth_files"],
                "exact_file_match": diff_result["exact_file_match"],
            },
            "agent_diff": agent_diff,
            "ground_truth_diff": ground_truth_diff,
            "project_metadata": project_metadata,
            "bobbin_metadata": bobbin_metadata,
            "bobbin_metrics": bobbin_metrics,
            "injection_result": injection_result,
            "config_overrides": config_overrides,
            "tool_use_summary": agent_result.get("tool_use_summary"),
            "output_summary": _extract_output_summary(agent_result),
        }

        path = _save_result(result, results_dir, run_id=run_id)
        status = "PASS" if test_result["passed"] else "FAIL"
        cost_str = ""
        if result["agent_result"].get("cost_usd") is not None:
            cost_str = f" ${result['agent_result']['cost_usd']:.2f}"
        click.echo(
            f"    {status} | precision={diff_result['file_precision']:.2f} "
            f"recall={diff_result['file_recall']:.2f} "
            f"f1={diff_result['f1']:.2f} | {agent_result['duration_seconds']:.0f}s{cost_str}"
        )
        click.echo(f"    Saved: {path.name}")

    return result


def _read_bobbin_metrics(
    ws: Path, source_tag: str, ground_truth_files: list[str],
) -> dict | None:
    """Read .bobbin/metrics.jsonl and summarize injection/gate/dedup events.

    Computes injection-to-ground-truth file overlap when ground truth is available.
    """
    metrics_path = ws / ".bobbin" / "metrics.jsonl"
    if not metrics_path.exists():
        return None

    try:
        lines = metrics_path.read_text(encoding="utf-8").splitlines()
    except OSError:
        return None

    events = []
    for line in lines:
        line = line.strip()
        if not line:
            continue
        try:
            ev = json.loads(line)
            if ev.get("source") == source_tag:
                events.append(ev)
        except json.JSONDecodeError:
            continue

    if not events:
        return None

    injections = [e for e in events if e.get("event_type") == "hook_injection"]
    gate_skips = [e for e in events if e.get("event_type") == "hook_gate_skip"]
    dedup_skips = [e for e in events if e.get("event_type") == "hook_dedup_skip"]
    commands = [e for e in events if e.get("event_type") == "command"]
    prime_events = [e for e in events if e.get("event_type") == "hook_prime_context"]

    # Collect all injected files across all injections.
    injected_files: set[str] = set()
    for inj in injections:
        for f in inj.get("metadata", {}).get("files_returned", []):
            injected_files.add(f)

    # Extract per-event gate skip details (score, query, threshold).
    gate_skip_details = []
    for gs in gate_skips:
        meta = gs.get("metadata", {})
        gate_skip_details.append({
            "query": meta.get("query", ""),
            "top_score": meta.get("top_score"),
            "gate_threshold": meta.get("gate_threshold"),
        })

    # Normalize injected file paths to workspace-relative for overlap comparison.
    ws_str = str(ws)
    injected_files_rel: set[str] = set()
    for f in injected_files:
        if f.startswith(ws_str):
            rel = f[len(ws_str):].lstrip("/")
            injected_files_rel.add(rel)
        else:
            injected_files_rel.add(f)

    result: dict = {
        "injection_count": len(injections),
        "gate_skip_count": len(gate_skips),
        "gate_skip_details": gate_skip_details,
        "dedup_skip_count": len(dedup_skips),
        "prime_context_fired": len(prime_events) > 0,
        "command_invocations": [
            {"command": e.get("command", ""), "duration_ms": e.get("duration_ms", 0)}
            for e in commands
        ],
        "injected_files": sorted(injected_files_rel),
        "total_events": len(events),
    }

    # Compute injection-to-ground-truth overlap using normalized relative paths.
    if injected_files_rel and ground_truth_files:
        gt_set = set(ground_truth_files)
        overlap = injected_files_rel & gt_set
        result["overlap"] = {
            "injection_precision": round(len(overlap) / len(injected_files_rel), 4) if injected_files_rel else 0,
            "injection_recall": round(len(overlap) / len(gt_set), 4) if gt_set else 0,
            "overlap_files": sorted(overlap),
        }

    return result


def _extract_cost_metrics(agent_result: dict) -> dict:
    """Extract cost and token metrics from Claude Code's JSON output.

    Claude Code returns ``total_cost_usd``, ``usage`` (with token counts),
    and ``modelUsage`` (per-model breakdowns) in its JSON result.  We pull
    these into flat keys suitable for aggregation in reports.
    """
    parsed = agent_result.get("result") or {}
    if not isinstance(parsed, dict):
        return {}

    metrics: dict = {}

    # Total cost.
    if "total_cost_usd" in parsed:
        metrics["cost_usd"] = parsed["total_cost_usd"]

    # Aggregate token counts from usage block.
    usage = parsed.get("usage")
    if isinstance(usage, dict):
        input_tokens = usage.get("input_tokens", 0)
        output_tokens = usage.get("output_tokens", 0)
        cache_read = usage.get("cache_read_input_tokens", 0)
        cache_creation = usage.get("cache_creation_input_tokens", 0)
        metrics["input_tokens"] = input_tokens + cache_read + cache_creation
        metrics["output_tokens"] = output_tokens

    # Per-model breakdown (for transparency).
    model_usage = parsed.get("modelUsage")
    if isinstance(model_usage, dict):
        metrics["model_usage"] = model_usage

    # Number of turns.
    if "num_turns" in parsed:
        metrics["num_turns"] = parsed["num_turns"]

    return metrics


def _extract_output_summary(agent_result: dict) -> str | None:
    """Extract a short summary of what the agent did from its output.

    Claude Code's JSON result includes a ``result`` field with the agent's
    final text response.  We capture the first 2000 characters as a summary.
    """
    parsed = agent_result.get("result")
    if not isinstance(parsed, dict):
        return None

    # The agent's conversational output is in the "result" key of the
    # parsed JSON (Claude Code nests it as result.result).
    text = parsed.get("result", "")
    if isinstance(text, str) and text.strip():
        return text[:2000]
    return None


def _resolve_approaches(approaches: str) -> list[str]:
    """Expand 'both' into the two approach names."""
    if approaches == "both":
        return ["no-bobbin", "with-bobbin"]
    return [approaches]


@click.group()
@click.option("-v", "--verbose", is_flag=True, help="Enable debug logging.")
def cli(verbose: bool):
    """Bobbin evaluation framework — compare Claude Code with and without bobbin."""
    _setup_logging(verbose)


@cli.command()
@click.argument("task_id")
@click.option("--attempts", default=3, help="Number of attempts per approach.")
@click.option(
    "--approaches",
    default="both",
    type=click.Choice(["no-bobbin", "with-bobbin", "both"]),
    help="Which approaches to evaluate.",
)
@click.option("--tasks-dir", default="tasks", help="Directory containing task YAML files.")
@click.option("--results-dir", default="results", help="Directory to store result JSON files.")
@click.option("--settings-file", default=None, help="Claude Code settings file for with-bobbin.")
@click.option("--model", default="claude-opus-4-6", help="Claude model to use.")
@click.option("--budget", default=100.00, type=float, help="Max budget per run (USD).")
@click.option("--timeout", default=3600, type=int, help="Agent timeout in seconds.")
@click.option("--index-timeout", default=600, type=int, help="Bobbin index timeout in seconds.")
@click.option("--skip-verify", is_flag=True, help="Skip test verification at parent commit.")
@click.option("--save-stream/--no-save-stream", default=True, help="Save raw JSONL stream alongside results.")
@click.option("--force-budget", is_flag=True, help="Skip session budget check (run even if token is low).")
@click.option(
    "--config-overrides", "-C",
    multiple=True,
    help=(
        "Override bobbin config for ablation testing (repeatable). "
        "Each KEY=VALUE creates a separate ablation approach. "
        "Supported keys: semantic_weight, coupling_depth, gate_threshold, "
        "doc_demotion, recency_weight, blame_bridging. "
        "Example: -C semantic_weight=0.0 -C gate_threshold=1.0"
    ),
)
def run_task(
    task_id: str,
    attempts: int,
    approaches: str,
    tasks_dir: str,
    results_dir: str,
    settings_file: str | None,
    model: str,
    budget: float,
    timeout: int,
    index_timeout: int,
    skip_verify: bool,
    save_stream: bool,
    force_budget: bool,
    config_overrides: tuple[str, ...],
):
    """Run evaluation for a single task.

    TASK_ID is the task identifier (e.g., ruff-001).

    Use --config-overrides/-C for ablation testing.  Each override creates a
    separate with-bobbin variant where one parameter is changed, isolating its
    effect on eval outcomes.

    \b
    Examples:
        bobbin-eval run-task ruff-001 -C semantic_weight=0.0
        bobbin-eval run-task ruff-001 -C semantic_weight=0.0 -C gate_threshold=1.0
    """
    _check_session_budget(force=force_budget)

    tasks_path = _resolve_tasks_dir(tasks_dir)
    rdir = Path(results_dir)

    # Auto-resolve settings file for with-bobbin runs.
    if settings_file is None and _DEFAULT_SETTINGS.exists():
        settings_file = str(_DEFAULT_SETTINGS)

    # Parse config overrides into ablation variants.
    override_variants = _parse_override_specs(config_overrides) if config_overrides else []

    try:
        task = load_task_by_id(task_id, tasks_path)
    except TaskLoadError as exc:
        click.echo(f"Error: {exc}", err=True)
        sys.exit(1)

    approach_list = _resolve_approaches(approaches)
    # Each override variant adds an ablation approach.
    ablation_approaches = [
        (_override_approach_name(v), v) for v in override_variants
    ]
    all_approach_names = approach_list + [name for name, _ in ablation_approaches]
    total = len(all_approach_names) * attempts
    run_id = _generate_run_id()
    started_at = datetime.now(timezone.utc).isoformat()
    click.echo(
        f"Running task {task_id}: {total} runs "
        f"({len(all_approach_names)} approaches × {attempts} attempts) "
        f"[run {run_id}]"
    )
    if ablation_approaches:
        click.echo(f"  Ablation variants: {[n for n, _ in ablation_approaches]}")

    results = []
    # Run standard approaches (no-bobbin, with-bobbin).
    for approach in approach_list:
        for attempt in range(attempts):
            result = _run_single(
                task,
                approach,
                attempt,
                rdir,
                run_id=run_id,
                settings_file=settings_file,
                model=model,
                budget=budget,
                timeout=timeout,
                index_timeout=index_timeout,
                skip_verify=skip_verify,
                save_stream=save_stream,
            )
            results.append(result)

    # Run ablation approaches (with-bobbin + one override each).
    for approach_name, overrides in ablation_approaches:
        for attempt in range(attempts):
            result = _run_single(
                task,
                approach_name,
                attempt,
                rdir,
                run_id=run_id,
                settings_file=settings_file,
                model=model,
                budget=budget,
                timeout=timeout,
                index_timeout=index_timeout,
                skip_verify=skip_verify,
                save_stream=save_stream,
                config_overrides=overrides,
            )
            results.append(result)

    completed_at = datetime.now(timezone.utc).isoformat()
    completed_count = sum(1 for r in results if r.get("status") == "completed")
    _write_manifest(
        rdir,
        run_id,
        started_at=started_at,
        completed_at=completed_at,
        model=model,
        budget=budget,
        timeout=timeout,
        index_timeout=index_timeout,
        attempts_per_approach=attempts,
        approaches=all_approach_names,
        tasks=[task_id],
        total_results=len(results),
        completed_results=completed_count,
    )

    passed = sum(1 for r in results if r.get("test_result", {}).get("passed"))
    click.echo(f"\nDone: {passed}/{len(results)} runs passed tests [run {run_id}]")


@cli.command()
@click.option("--tasks-dir", default="tasks", help="Directory containing task YAML files.")
@click.option("--results-dir", default="results", help="Directory to store result JSON files.")
@click.option("--attempts", default=3, type=int, help="Number of attempts per approach.")
@click.option(
    "--approaches",
    default="both",
    type=click.Choice(["no-bobbin", "with-bobbin", "both"]),
)
@click.option("--settings-file", default=None, help="Claude Code settings file for with-bobbin.")
@click.option("--model", default="claude-opus-4-6", help="Claude model to use.")
@click.option("--budget", default=100.00, type=float, help="Max budget per run (USD).")
@click.option("--timeout", default=3600, type=int, help="Agent timeout in seconds.")
@click.option("--index-timeout", default=600, type=int, help="Bobbin index timeout in seconds.")
@click.option("--skip-verify", is_flag=True, help="Skip test verification at parent commit.")
@click.option("--save-stream/--no-save-stream", default=True, help="Save raw JSONL stream alongside results.")
@click.option("--force-budget", is_flag=True, help="Skip session budget check (run even if token is low).")
@click.option(
    "--config-overrides", "-C",
    multiple=True,
    help=(
        "Override bobbin config for ablation testing (repeatable). "
        "Each KEY=VALUE creates a separate ablation approach. "
        "Supported keys: semantic_weight, coupling_depth, gate_threshold, "
        "doc_demotion, recency_weight, blame_bridging. "
        "Example: -C semantic_weight=0.0 -C gate_threshold=1.0"
    ),
)
def run_all(
    tasks_dir: str,
    results_dir: str,
    attempts: int,
    approaches: str,
    settings_file: str | None,
    model: str,
    budget: float,
    timeout: int,
    index_timeout: int,
    skip_verify: bool,
    save_stream: bool,
    force_budget: bool,
    config_overrides: tuple[str, ...],
):
    """Run evaluation for all tasks in the tasks directory.

    Use --config-overrides/-C for ablation testing.  See ``run-task --help``
    for details.
    """
    _check_session_budget(force=force_budget)

    tasks_path = _resolve_tasks_dir(tasks_dir)
    rdir = Path(results_dir)

    # Auto-resolve settings file for with-bobbin runs.
    if settings_file is None and _DEFAULT_SETTINGS.exists():
        settings_file = str(_DEFAULT_SETTINGS)

    # Parse config overrides into ablation variants.
    override_variants = _parse_override_specs(config_overrides) if config_overrides else []

    try:
        tasks = load_all_tasks(tasks_path)
    except TaskLoadError as exc:
        click.echo(f"Error: {exc}", err=True)
        sys.exit(1)

    approach_list = _resolve_approaches(approaches)
    ablation_approaches = [
        (_override_approach_name(v), v) for v in override_variants
    ]
    all_approach_names = approach_list + [name for name, _ in ablation_approaches]
    total = len(tasks) * len(all_approach_names) * attempts
    run_id = _generate_run_id()
    started_at = datetime.now(timezone.utc).isoformat()
    click.echo(
        f"Running {len(tasks)} tasks: {total} total runs "
        f"({len(all_approach_names)} approaches × {attempts} attempts) "
        f"[run {run_id}]"
    )
    if ablation_approaches:
        click.echo(f"  Ablation variants: {[n for n, _ in ablation_approaches]}")

    all_results = []
    for task in tasks:
        click.echo(f"\n--- {task['id']}: {task['repo']} ---")
        # Standard approaches.
        for approach in approach_list:
            for attempt in range(attempts):
                result = _run_single(
                    task,
                    approach,
                    attempt,
                    rdir,
                    run_id=run_id,
                    settings_file=settings_file,
                    model=model,
                    budget=budget,
                    timeout=timeout,
                    index_timeout=index_timeout,
                    skip_verify=skip_verify,
                    save_stream=save_stream,
                )
                all_results.append(result)
        # Ablation approaches.
        for approach_name, overrides in ablation_approaches:
            for attempt in range(attempts):
                result = _run_single(
                    task,
                    approach_name,
                    attempt,
                    rdir,
                    run_id=run_id,
                    settings_file=settings_file,
                    model=model,
                    budget=budget,
                    timeout=timeout,
                    index_timeout=index_timeout,
                    skip_verify=skip_verify,
                    save_stream=save_stream,
                    config_overrides=overrides,
                )
                all_results.append(result)

    completed_at = datetime.now(timezone.utc).isoformat()
    completed_count = sum(1 for r in all_results if r.get("status") == "completed")
    task_ids = [t["id"] for t in tasks]
    _write_manifest(
        rdir,
        run_id,
        started_at=started_at,
        completed_at=completed_at,
        model=model,
        budget=budget,
        timeout=timeout,
        index_timeout=index_timeout,
        attempts_per_approach=attempts,
        approaches=all_approach_names,
        tasks=task_ids,
        total_results=len(all_results),
        completed_results=completed_count,
    )

    passed = sum(1 for r in all_results if r.get("test_result", {}).get("passed"))
    click.echo(f"\nAll done: {passed}/{len(all_results)} runs passed tests [run {run_id}]")


@cli.command()
@click.argument("results_dir", default="results")
@click.option("--task", "-t", multiple=True, help="Filter to specific task ID(s). Repeatable.")
@click.option("--include-quarantined", is_flag=True, default=False,
              help="Include results for quarantined tasks (excluded by default).")
@click.option("--mixed-models", is_flag=True, default=False,
              help="Compare arms served by different models anyway; every arm "
                   "is labeled with its serving-model mix instead of refusing.")
def score(results_dir: str, task: tuple[str, ...], include_quarantined: bool,
          mixed_models: bool):
    """Display a summary of existing results.

    RESULTS_DIR is the directory containing result JSON files.

    By default, results for quarantined tasks (those only in tasks/_quarantined/)
    are excluded. Use --include-quarantined to show them.

    Serving model is part of a run's identity (bobbin-daa): runs whose
    artifacts carry no serving-model attribution are excluded from the
    comparison (with a reported count), and arms served by different models
    are refused unless --mixed-models forces a labeled, model-mixed table.
    """
    rdir = Path(results_dir)
    if not rdir.is_dir():
        click.echo(f"Error: Results directory not found: {rdir}", err=True)
        sys.exit(1)

    # Build set of quarantined task IDs (tasks in _quarantined/ but not in main dir).
    quarantined_ids: set[str] = set()
    if not include_quarantined:
        tasks_path = _resolve_tasks_dir("tasks")
        active_ids = {p.stem for p in tasks_path.glob("*.yaml")}
        quarantine_dir = tasks_path / "_quarantined"
        if quarantine_dir.is_dir():
            quarantined_ids = {p.stem for p in quarantine_dir.glob("*.yaml")} - active_ids

    # Load from run-based and legacy layouts.
    results: list[dict] = []
    runs_dir = rdir / "runs"
    if runs_dir.is_dir():
        for f in sorted(runs_dir.glob("*/*.json")):
            try:
                data = json.loads(f.read_text(encoding="utf-8"))
                if isinstance(data, dict) and "task_id" in data:
                    results.append(data)
            except (json.JSONDecodeError, OSError) as exc:
                click.echo(f"Warning: skipping {f.name}: {exc}", err=True)
    for f in sorted(rdir.glob("*.json")):
        try:
            data = json.loads(f.read_text(encoding="utf-8"))
            if isinstance(data, dict) and "task_id" in data:
                results.append(data)
        except (json.JSONDecodeError, OSError) as exc:
            click.echo(f"Warning: skipping {f.name}: {exc}", err=True)

    # Apply filters.
    if task:
        results = [r for r in results if r.get("task_id") in task]
    if quarantined_ids:
        before = len(results)
        results = [r for r in results if r.get("task_id") not in quarantined_ids]
        skipped = before - len(results)
        if skipped:
            click.echo(f"(excluded {skipped} quarantined results; use --include-quarantined to show)", err=True)

    if not results:
        click.echo(f"No result files found in {rdir}", err=True)
        sys.exit(1)

    # Serving-model identity (bobbin-daa): a run whose own artifact does not
    # name the model that served it cannot take part in a cross-arm
    # comparison — it is excluded and counted, never assumed to match.
    attributed, unattributed = partition_by_attribution(results)
    if unattributed:
        click.echo(
            f"(excluded {len(unattributed)} run(s) without serving-model "
            f"attribution in their artifacts from the comparison)",
            err=True,
        )
    results = attributed
    if not results:
        click.echo(
            "No attributed results remain to compare. Older artifacts without "
            "agent usage records cannot be re-attributed.",
            err=True,
        )
        sys.exit(1)

    # Group by approach and compute stats.
    by_approach: dict[str, list[dict]] = {}
    for r in results:
        a = r.get("approach", "unknown")
        by_approach.setdefault(a, []).append(r)

    # Refuse to pool arms served by different models unless the caller
    # explicitly asks for a labeled model-mixed table.
    arm_models = {a: serving_model_counts(runs) for a, runs in by_approach.items()}
    is_mixed = False
    try:
        check_comparable(by_approach)
    except ServingModelMismatch as exc:
        if not mixed_models:
            click.echo(f"Error: {exc}", err=True)
            click.echo(
                "Use --mixed-models to compare anyway; the output will be "
                "labeled model-mixed per arm.",
                err=True,
            )
            sys.exit(1)
        is_mixed = True
        click.echo(
            "WARNING: MODEL-MIXED COMPARISON — the arms below were served by "
            "different models; deltas are not attributable to the approach "
            "alone (docs/plans/paper-statistics.md §4b).",
            err=True,
        )

    if not is_mixed:
        only_model = next(iter({m for c in arm_models.values() for m in c}), "unknown")
        click.echo(f"Serving model: {only_model} ({len(results)} attributed runs)")

    click.echo(f"\n{'Approach':<16} {'Runs':>5} {'Pass':>5} {'Rate':>8} "
               f"{'Prec':>8} {'Recall':>8} {'F1':>8} {'Avg Time':>10}")
    click.echo("-" * 80)

    for approach in sorted(by_approach):
        runs = by_approach[approach]
        n = len(runs)
        passed = sum(1 for r in runs if r.get("test_result", {}).get("passed"))
        rate = passed / n if n else 0

        precisions = [
            r["diff_result"]["file_precision"]
            for r in runs
            if r.get("diff_result", {}).get("file_precision") is not None
        ]
        recalls = [
            r["diff_result"]["file_recall"]
            for r in runs
            if r.get("diff_result", {}).get("file_recall") is not None
        ]
        f1s = [
            r["diff_result"]["f1"]
            for r in runs
            if r.get("diff_result", {}).get("f1") is not None
        ]
        durations = [
            r["agent_result"]["duration_seconds"]
            for r in runs
            if r.get("agent_result", {}).get("duration_seconds") is not None
        ]

        avg_prec = sum(precisions) / len(precisions) if precisions else 0
        avg_recall = sum(recalls) / len(recalls) if recalls else 0
        avg_f1 = sum(f1s) / len(f1s) if f1s else 0
        avg_time = sum(durations) / len(durations) if durations else 0

        line = (
            f"{approach:<16} {n:>5} {passed:>5} {rate:>7.1%} "
            f"{avg_prec:>8.3f} {avg_recall:>8.3f} {avg_f1:>8.3f} {avg_time:>9.1f}s"
        )

        # Append injection usage metrics for with-bobbin runs.
        inj_f1s = [
            r["injection_result"]["injection_f1"]
            for r in runs
            if (r.get("injection_result") or {}).get("injection_f1") is not None
        ]
        if inj_f1s:
            avg_inj_f1 = sum(inj_f1s) / len(inj_f1s)
            line += f"  inj_f1={avg_inj_f1:.3f}"

        if is_mixed:
            rendered = ", ".join(f"{m}: {c}" for m, c in sorted(arm_models[approach].items()))
            line += f"  [models: {rendered}]"

        click.echo(line)


@cli.command()
@click.argument("results_dir", default="results")
@click.option("--output", "-o", default=None, help="Output path for the markdown report.")
def report(results_dir: str, output: str | None):
    """Generate a markdown summary report from results.

    RESULTS_DIR is the directory containing result JSON files.
    """
    from analysis.report import ReportError, generate_report

    if output is None:
        output = str(Path(results_dir) / "report.md")

    try:
        generate_report(results_dir, output)
    except ReportError as exc:
        click.echo(f"Error: {exc}", err=True)
        sys.exit(1)

    click.echo(f"Report written to {output}")


def _load_results(results_dir: Path) -> list[dict]:
    """Load all JSON result files from the results directory.

    Scans ``results/runs/*/*.json`` first (run-based layout), then falls
    back to ``results/*.json`` (legacy flat layout).  Manifest and judge
    files are skipped via the ``task_id`` check.
    """
    results: list[dict] = []
    seen_paths: set[Path] = set()

    # Run-based layout: results/runs/<run_id>/<task>_<approach>_<attempt>.json
    runs_dir = results_dir / "runs"
    if runs_dir.is_dir():
        for f in sorted(runs_dir.glob("*/*.json")):
            seen_paths.add(f.resolve())
            try:
                data = json.loads(f.read_text(encoding="utf-8"))
                if isinstance(data, dict) and data.get("status") == "completed":
                    results.append(data)
            except (json.JSONDecodeError, OSError):
                pass

    # Legacy flat layout: results/*.json
    for f in sorted(results_dir.glob("*.json")):
        if f.resolve() in seen_paths:
            continue
        try:
            data = json.loads(f.read_text(encoding="utf-8"))
            if isinstance(data, dict) and data.get("status") == "completed":
                results.append(data)
        except (json.JSONDecodeError, OSError):
            pass

    return results


def _group_results_by_task(results: list[dict]) -> dict[str, dict[str, list[dict]]]:
    """Group results by task_id then by approach.

    Returns ``{task_id: {approach: [results]}}``.
    """
    grouped: dict[str, dict[str, list[dict]]] = {}
    for r in results:
        tid = r.get("task_id", "unknown")
        approach = r.get("approach", "unknown")
        grouped.setdefault(tid, {}).setdefault(approach, []).append(r)
    return grouped


def _pick_best_attempt(runs: list[dict]) -> dict | None:
    """Pick the best attempt from a list of runs for a single task×approach.

    Prefers passing runs, then highest F1, then first attempt.
    """
    if not runs:
        return None
    passing = [r for r in runs if r.get("test_result", {}).get("passed")]
    pool = passing if passing else runs
    return max(pool, key=lambda r: r.get("diff_result", {}).get("f1", 0.0))


@cli.command()
@click.argument("results_dir", default="results")
@click.option(
    "--judge-model",
    default="claude-sonnet-4-5-20250929",
    help="Model to use as the LLM judge.",
)
@click.option(
    "--pairs",
    default="all",
    type=click.Choice(["all", "ai-vs-ai", "human-vs-ai", "human-vs-bobbin"]),
    help="Which pairs to judge.",
)
@click.option("--run", "run_id", default=None, help="Run ID to load/save results from.")
def judge(results_dir: str, judge_model: str, pairs: str, run_id: str | None):
    """Run LLM-as-judge pairwise comparison on stored results.

    Compares three pairs per task:
      - human (ground truth) vs AI (no-bobbin)
      - human (ground truth) vs AI+bobbin (with-bobbin)
      - AI vs AI+bobbin

    Requires that results contain stored diffs (agent_diff + ground_truth_diff).
    Judge results are saved alongside the original results.
    """
    from scorer.llm_judge import LLMJudgeError, judge_pairwise

    rdir = Path(results_dir)
    if not rdir.is_dir():
        click.echo(f"Error: Results directory not found: {rdir}", err=True)
        sys.exit(1)

    # When --run is specified, scope loading and saving to that run directory.
    if run_id:
        run_dir = rdir / "runs" / run_id
        if not run_dir.is_dir():
            click.echo(f"Error: Run directory not found: {run_dir}", err=True)
            sys.exit(1)
        results = _load_results(run_dir)
    else:
        results = _load_results(rdir)

    if not results:
        click.echo(f"Error: No completed results in {rdir}", err=True)
        sys.exit(1)

    # Check that results have stored diffs.
    has_diffs = any("agent_diff" in r for r in results)
    if not has_diffs:
        click.echo(
            "Error: Results do not contain stored diffs (agent_diff). "
            "Re-run evaluations with the updated runner to capture diffs.",
            err=True,
        )
        sys.exit(1)

    grouped = _group_results_by_task(results)
    all_judgements: list[dict] = []

    for task_id, by_approach in sorted(grouped.items()):
        click.echo(f"\n--- Judging {task_id} ---")

        no_bobbin = _pick_best_attempt(by_approach.get("no-bobbin", []))
        with_bobbin = _pick_best_attempt(by_approach.get("with-bobbin", []))

        if not no_bobbin and not with_bobbin:
            click.echo(f"  Skipping {task_id}: no completed runs")
            continue

        # Build context for the judge.
        sample = no_bobbin or with_bobbin
        task_info = sample.get("task", {})
        context = {
            "repo": task_info.get("repo", ""),
            "description": task_info.get("description", task_id),
            "language": task_info.get("language", ""),
        }

        gt_diff = (sample or {}).get("ground_truth_diff", "")

        # Define pairs to judge.
        pair_configs = []
        if pairs in ("all", "ai-vs-ai") and no_bobbin and with_bobbin:
            pair_configs.append({
                "name": "AI vs AI+bobbin",
                "label": "ai-vs-ai+bobbin",
                "diff_a": no_bobbin.get("agent_diff", ""),
                "diff_b": with_bobbin.get("agent_diff", ""),
                "a_label": "no-bobbin",
                "b_label": "with-bobbin",
            })
        if pairs in ("all", "human-vs-ai") and no_bobbin and gt_diff:
            pair_configs.append({
                "name": "Human vs AI",
                "label": "human-vs-ai",
                "diff_a": gt_diff,
                "diff_b": no_bobbin.get("agent_diff", ""),
                "a_label": "human",
                "b_label": "no-bobbin",
            })
        if pairs in ("all", "human-vs-bobbin") and with_bobbin and gt_diff:
            pair_configs.append({
                "name": "Human vs AI+bobbin",
                "label": "human-vs-ai+bobbin",
                "diff_a": gt_diff,
                "diff_b": with_bobbin.get("agent_diff", ""),
                "a_label": "human",
                "b_label": "with-bobbin",
            })

        for pair in pair_configs:
            if not pair["diff_a"].strip() or not pair["diff_b"].strip():
                click.echo(f"  Skipping {pair['name']}: empty diff(s)")
                continue

            click.echo(f"  Judging: {pair['name']}...")
            try:
                verdict = judge_pairwise(
                    pair["diff_a"],
                    pair["diff_b"],
                    context,
                    model=judge_model,
                )
            except LLMJudgeError as exc:
                click.echo(f"    Judge error: {exc}", err=True)
                continue

            # Map winner labels back to meaningful names.
            winner_map = {"a": pair["a_label"], "b": pair["b_label"], "tie": "tie"}
            named_winner = winner_map.get(verdict["overall_winner"], verdict["overall_winner"])

            judgement = {
                "task_id": task_id,
                "pair": pair["label"],
                "a_label": pair["a_label"],
                "b_label": pair["b_label"],
                "overall_winner": verdict["overall_winner"],
                "named_winner": named_winner,
                "dimensions": verdict["dimensions"],
                "bias_detected": verdict.get("bias_detected", False),
                "reasoning": verdict.get("reasoning", ""),
                "judge_model": judge_model,
                "timestamp": datetime.now(timezone.utc).isoformat(),
            }
            all_judgements.append(judgement)

            click.echo(
                f"    Winner: {named_winner}"
                f" (bias={'yes' if verdict.get('bias_detected') else 'no'})"
            )

    # Save all judgements.
    if all_judgements:
        save_dir = rdir / "runs" / run_id if run_id else rdir
        save_dir.mkdir(parents=True, exist_ok=True)
        judge_file = save_dir / "judge_results.json"
        judge_file.write_text(
            json.dumps(all_judgements, indent=2, default=str),
            encoding="utf-8",
        )
        click.echo(f"\nJudge results saved to {judge_file}")
        click.echo(f"Total judgements: {len(all_judgements)}")

        # Print summary.
        click.echo("\nJudge Summary:")
        for pair_label in ("ai-vs-ai+bobbin", "human-vs-ai", "human-vs-ai+bobbin"):
            pair_results = [j for j in all_judgements if j["pair"] == pair_label]
            if not pair_results:
                continue
            wins: dict[str, int] = {}
            for j in pair_results:
                w = j["named_winner"]
                wins[w] = wins.get(w, 0) + 1
            total = len(pair_results)
            parts = [f"{name}: {count}/{total}" for name, count in sorted(wins.items())]
            click.echo(f"  {pair_label}: {', '.join(parts)}")
    else:
        click.echo("\nNo judgements were produced.")


@cli.command()
@click.argument("results_dir", default="results")
@click.option(
    "--output-dir",
    default=None,
    help="Output directory for generated pages (default: docs/book/src/eval).",
)
@click.option("--tasks-dir", default="tasks", help="Directory containing task YAML files.")
@click.option("--run", "run_id", default=None, help="Publish a specific run (default: latest).")
@click.option("--all-runs", is_flag=True, help="Include historical trends from all runs.")
def publish(results_dir: str, output_dir: str | None, tasks_dir: str, run_id: str | None, all_runs: bool):
    """Generate mdbook pages from eval results.

    Reads results JSON files and task definitions, then generates markdown
    pages with matplotlib SVG charts for the documentation site.
    """
    from analysis.mdbook_pages import PageGenError, generate_all_pages

    if output_dir is None:
        output_dir = str(_EVAL_ROOT.parent / "docs" / "book" / "src" / "eval")

    tasks_path = _resolve_tasks_dir(tasks_dir)

    try:
        generated = generate_all_pages(
            results_dir,
            str(tasks_path),
            output_dir,
            run_id=run_id,
            include_trends=all_runs,
        )
    except PageGenError as exc:
        click.echo(f"Error: {exc}", err=True)
        sys.exit(1)

    click.echo(f"Generated {len(generated)} pages in {output_dir}:")
    for f in generated:
        click.echo(f"  {f}")


def _parse_float_list(value: str) -> list[float]:
    """Parse comma-separated float values."""
    return [float(x.strip()) for x in value.split(",") if x.strip()]


@cli.command()
@click.option("--tasks-dir", default="tasks", help="Directory containing task YAML files.")
@click.option("--task", "task_id", default=None, help="Calibrate a single task (by ID).")
@click.option(
    "--repos",
    default=None,
    help="Comma-separated repo prefixes to include (e.g. ruff,cargo).",
)
@click.option(
    "--semantic-weights",
    default="0.5,0.6,0.7,0.8,0.9",
    help="Comma-separated semantic_weight values to test.",
)
@click.option(
    "--doc-demotions",
    default="0.3,0.5,0.7,1.0",
    help="Comma-separated doc_demotion values to test.",
)
@click.option(
    "--rrf-ks",
    default="20.0,40.0,60.0,80.0",
    help="Comma-separated RRF k values to test.",
)
@click.option(
    "--ppr-weights",
    default="0.0",
    help="Comma-separated ppr_weight values to test (requires a knowledge-feature "
    "bobbin build + populated Quipu coupling graph). 0.0 = PPR off.",
)
@click.option(
    "--output",
    "-o",
    default=None,
    help="Output path for calibration report (default: results/calibration.md).",
)
@click.option(
    "--json-output",
    default=None,
    help="Output path for raw JSON results.",
)
@click.option("--index-timeout", default=600, type=int, help="Bobbin index timeout in seconds.")
@click.option("--max-tasks", default=0, type=int, help="Max tasks to calibrate (0=all).")
def calibrate(
    tasks_dir: str,
    task_id: str | None,
    repos: str | None,
    semantic_weights: str,
    doc_demotions: str,
    rrf_ks: str,
    ppr_weights: str,
    output: str | None,
    json_output: str | None,
    index_timeout: int,
    max_tasks: int,
):
    """Calibrate search weights by sweeping parameter configs against eval tasks.

    Indexes each repo once, then runs bobbin context with every parameter
    combination and measures precision/recall of returned files vs ground
    truth. No LLM calls — pure search quality measurement.

    Example::

        bobbin-eval calibrate --task ruff-001 --semantic-weights 0.5,0.7,0.9
        bobbin-eval calibrate --repos ruff --rrf-ks 40,60,80
    """
    from runner.calibrate import (
        CalibrationError,
        ParamConfig,
        build_param_grid,
        run_calibration,
    )

    tasks_path = _resolve_tasks_dir(tasks_dir)

    # Load tasks
    if task_id:
        try:
            tasks = [load_task_by_id(task_id, tasks_path)]
        except TaskLoadError as exc:
            click.echo(f"Error: {exc}", err=True)
            sys.exit(1)
    else:
        try:
            tasks = load_all_tasks(tasks_path)
        except TaskLoadError as exc:
            click.echo(f"Error: {exc}", err=True)
            sys.exit(1)

    # Filter by repo prefix
    if repos:
        repo_prefixes = [r.strip().lower() for r in repos.split(",")]
        tasks = [
            t for t in tasks
            if any(t["id"].lower().startswith(p) for p in repo_prefixes)
        ]

    if max_tasks > 0:
        tasks = tasks[:max_tasks]

    if not tasks:
        click.echo("No tasks matched the filters.", err=True)
        sys.exit(1)

    # Build parameter grid
    ppr_values = _parse_float_list(ppr_weights)
    sw_values = _parse_float_list(semantic_weights)
    dd_values = _parse_float_list(doc_demotions)
    k_values = _parse_float_list(rrf_ks)
    configs = build_param_grid(sw_values, dd_values, k_values, ppr_values)

    click.echo(
        f"Calibrating {len(tasks)} tasks × {len(configs)} configs "
        f"= {len(tasks) * len(configs)} probes"
    )
    click.echo(
        f"  semantic_weight: {sw_values}\n"
        f"  doc_demotion:    {dd_values}\n"
        f"  rrf_k:           {k_values}"
    )

    # Run calibration
    report = run_calibration(
        tasks, configs, index_timeout=index_timeout
    )

    # Output report
    if output is None:
        output = str(Path("results") / "calibration.md")
    Path(output).parent.mkdir(parents=True, exist_ok=True)
    md = report.to_markdown()
    Path(output).write_text(md, encoding="utf-8")
    click.echo(f"\nReport written to {output}")

    # Output JSON
    if json_output:
        Path(json_output).parent.mkdir(parents=True, exist_ok=True)
        json_data = {
            "configs": [
                {
                    "semantic_weight": c.semantic_weight,
                    "doc_demotion": c.doc_demotion,
                    "rrf_k": c.rrf_k,
                }
                for c in configs
            ],
            "results": [
                {
                    "task_id": r.task_id,
                    "config": r.config.label,
                    "precision": r.precision,
                    "recall": r.recall,
                    "f1": r.f1,
                    "top_semantic_score": r.top_semantic_score,
                    "returned_files": r.returned_files,
                    "ground_truth_files": r.ground_truth_files,
                    "duration_ms": r.duration_ms,
                }
                for r in report.results
            ],
            "summary": report.summary_by_config(),
        }
        Path(json_output).write_text(
            json.dumps(json_data, indent=2), encoding="utf-8"
        )
        click.echo(f"JSON results written to {json_output}")

    # Print summary table
    summaries = report.summary_by_config()
    if summaries:
        click.echo(f"\nTop configs (by F1, {len(report.results)} total probes):")
        click.echo(
            f"  {'Config':<40} {'P':>8} {'R':>8} {'F1':>8} {'TopSem':>8}"
        )
        click.echo("  " + "-" * 76)
        for s in summaries[:10]:
            click.echo(
                f"  {s['config']:<40} {s['avg_precision']:>8.3f} "
                f"{s['avg_recall']:>8.3f} {s['avg_f1']:>8.3f} "
                f"{s['avg_top_semantic_score']:>8.3f}"
            )
        if summaries:
            best = summaries[0]
            click.echo(f"\n  Best: {best['config']} (F1={best['avg_f1']:.4f})")
    else:
        click.echo("\nNo results collected.")


@cli.command("l0")
@click.option("--tasks-dir", default="tasks", help="Directory containing task YAML files.")
@click.option("--task", "task_ids", multiple=True, help="Restrict to these task ids (repeatable).")
@click.option("--arms", default="all", help="Comma-separated arms, or 'all'. See eval/v2/PREREGISTRATION.md.")
@click.option("--budgets", default="100,300,600", help="Comma-separated line budgets (300 is primary).")
@click.option("--out", "out_path", required=True, help="JSONL file to append arm scores to.")
@click.option("--workdir", default=None, help="Where to clone task repos (default: a temp dir).")
@click.option("--index-timeout", default=1800, type=int, help="Bobbin index timeout in seconds.")
@click.option("--reuse-model-dir", type=click.Path(path_type=Path, exists=True, file_okay=False),
              default=None, help="Opt-in sealed CPU-index reuse; exact isolated native model cache.")
def l0(tasks_dir, task_ids, arms, budgets, out_path, workdir, index_timeout, reuse_model_dir):
    """Eval v2 L0: offline retrieval through the production injection hook.

    No agent and no LLM calls. Scores each arm's would-be injection against the
    gold hunks of the task's fixing commit. Every bobbin call runs with no
    BOBBIN_* variables and an empty XDG config, so nothing can route to a
    shared server.
    """
    import subprocess as _sp

    from runner import l0 as L0
    from runner.bobbin_setup import _find_bobbin, setup_bobbin
    from runner.workspace import checkout_parent, clone_repo

    arm_list = list(L0.ALL_ARMS) if arms == "all" else [a.strip() for a in arms.split(",") if a.strip()]
    unknown = [a for a in arm_list if a not in L0.ALL_ARMS]
    if unknown:
        click.echo(f"Error: unknown arms {unknown}; known: {list(L0.ALL_ARMS)}", err=True)
        sys.exit(2)
    budget_list = [int(b) for b in budgets.split(",")]

    tasks_path = _resolve_tasks_dir(tasks_dir)
    try:
        tasks = [load_task_by_id(t, tasks_path) for t in task_ids] if task_ids else load_all_tasks(tasks_path)
    except TaskLoadError as exc:
        click.echo(f"Error: {exc}", err=True)
        sys.exit(1)

    scratch = Path(workdir) if workdir else L0.new_scratch()
    reuse_limits = None
    if reuse_model_dir is not None:
        from runner.index_reuse import check_scope
        reuse_limits = check_scope()
        if scratch.is_symlink() or (scratch.exists() and any(scratch.iterdir())):
            raise click.ClickException("sealed reuse requires a fresh empty run workspace")
    scratch.mkdir(parents=True, exist_ok=True)
    env = L0.isolated_env(scratch)
    # setup_bobbin inherits the process environment, so isolate it too.
    for k in [k for k in os.environ if k.startswith("BOBBIN_")]:
        del os.environ[k]
    os.environ["XDG_CONFIG_HOME"] = env["XDG_CONFIG_HOME"]
    bobbin = _find_bobbin()
    # Pin the actual executable for a long campaign. Host release automation
    # may replace the installed CLI while later tasks are still being indexed.
    pinned_dir = scratch / "bin"
    pinned_dir.mkdir(exist_ok=True)
    pinned_bobbin = pinned_dir / "bobbin"
    if not pinned_bobbin.exists():
        shutil.copy2(bobbin, pinned_bobbin)
    bobbin = str(pinned_bobbin.resolve())
    env["PATH"] = str(pinned_dir.resolve()) + os.pathsep + env["PATH"]
    os.environ["PATH"] = env["PATH"]

    out = Path(out_path)
    if reuse_model_dir is not None and out.exists():
        raise click.ClickException("sealed reuse requires a fresh run/output; never resume partial scores")
    prereg = Path(__file__).resolve().parent.parent / "v2" / "PREREGISTRATION.md"
    manifest = {
        "kind": "manifest",
        "bobbin_sha256": __import__("hashlib").sha256(Path(bobbin).read_bytes()).hexdigest(),
        "started_at": datetime.now(timezone.utc).isoformat(),
        "bobbin": _sp.run([bobbin, "--version"], capture_output=True, text=True, env=env).stdout.strip(),
        "harness_commit": _sp.run(
            ["git", "rev-parse", "HEAD"], cwd=Path(__file__).parent, capture_output=True, text=True
        ).stdout.strip(),
        "preregistration_sha256": __import__("hashlib").sha256(prereg.read_bytes()).hexdigest()
        if prereg.exists()
        else None,
        "arms": arm_list,
        "budgets": budget_list,
        "tasks": [t["id"] for t in tasks],
    }
    from runner.index_reuse import planned_cells
    cells = planned_cells(tasks, arm_list, budget_list)
    manifest["planned_cells"] = cells
    manifest["index_reuse"] = "sealed-no-coupling/v1" if reuse_model_dir else "legacy"
    manifest["scoring_paths"] = "workspace-qualified-or-literal/v1" if reuse_model_dir else "literal"
    manifest["hook_outcomes"] = L0.INDEPENDENT_HOOK_CONTRACT if reuse_model_dir else "legacy"
    if reuse_limits is not None:
        manifest["resource_limits"] = reuse_limits
        manifest["wall_supervisor_required_seconds"] = 21600
    out.parent.mkdir(parents=True, exist_ok=True)
    with out.open("a") as fh:
        fh.write(json.dumps(manifest) + "\n")

    derived_sources = {}
    observed_scores = []
    if reuse_model_dir is not None:
        from runner.index_reuse import write_coverage
        write_coverage(scratch / "matrix-coverage.json", cells, observed_scores)

    def index(ws: Path, overrides=None):
        if reuse_model_dir is not None:
            from runner.index_reuse import MARKER, ReuseRefused, derive_no_coupling, identity, seal, verify
            # Never reuse a pre-existing unsealed artifact or silently rebuild
            # after mismatch. The no-coupling copy is made before any hook runs.
            root = ws / ".bobbin"
            if (root / MARKER).exists():
                verify(ws, Path(bobbin), reuse_model_dir, overrides)
                return
            if overrides == {"git.coupling_enabled": "false"}:
                if ws not in derived_sources:
                    raise click.ClickException("unknown no-coupling workspace identity")
                source = derived_sources[ws]
                cost = derive_no_coupling(source, ws, Path(bobbin), reuse_model_dir)
            else:
                if overrides is not None or root.exists():
                    raise click.ClickException("unsealed index or unsupported arm; choose a fresh run")
                started = __import__("time").monotonic()
                admitted = []
                cost = setup_bobbin(str(ws), timeout=index_timeout,
                                    before_index=lambda path: admitted.append(
                                        identity(path, Path(bobbin), reuse_model_dir, None)))
                if admitted != [identity(ws, Path(bobbin), reuse_model_dir, None)]:
                    raise ReuseRefused("index input changed during build or admission was skipped")
                sealed = seal(ws, Path(bobbin), reuse_model_dir, None)
                cost.update(kind="cold", seconds=__import__("time").monotonic() - started,
                            payload_bytes=sealed["payload"]["bytes"], native_index_calls=1)
            with (scratch / "index-costs.jsonl").open("a") as fh:
                fh.write(json.dumps({"workspace": str(ws), **cost}) + "\n")
            return
        fingerprint = {
            "head": _sp.check_output(["git", "rev-parse", "HEAD"], cwd=ws, text=True).strip(),
            "bobbin_sha256": manifest["bobbin_sha256"], "overrides": overrides,
        }
        marker = ws / ".bobbin" / "eval-index-complete.json"
        if marker.exists() and json.loads(marker.read_text()) == fingerprint:
            return
        setup_bobbin(str(ws), timeout=index_timeout, config_overrides=overrides,
                     initialize=not (ws / ".bobbin/config.toml").exists())
        marker.write_text(json.dumps(fingerprint))

    for task in tasks:
        # clone_repo creates <dest>/<owner>--<name> and returns it; use that path,
        # and reuse an already-indexed checkout on a re-run.
        ws = scratch / task["id"] / task["repo"].replace("/", "--")
        if not (ws / ".git").exists():
            ws = clone_repo(task["repo"], str(scratch / task["id"]))
        checkout_parent(ws, task["commit"])
        index(ws)
        if reuse_model_dir is not None:
            from runner.cell_isolation import isolated_cell
            # Same basename for native labels/affinity, separate coupling data.
            fork = ws.parent / "nocoupling" / ws.name
            derived_sources[fork] = ws
            if "-coupling" in arm_list:
                shutil.copytree(ws, fork, symlinks=True, ignore=shutil.ignore_patterns(".bobbin"))
                index(fork, {"git.coupling_enabled": "false"})
            scores = []
            for budget in budget_list:
                for arm in arm_list:
                    target = fork if arm == "-coupling" else ws
                    overrides = {"git.coupling_enabled": "false"} if arm == "-coupling" else None
                    journal = scratch / "cell-state" / task["id"] / arm / str(budget)
                    try:
                        row = isolated_cell(
                            target, Path(bobbin), reuse_model_dir, overrides, journal,
                            lambda a=arm, b=budget: L0.run_task(
                                task, ws, [a], [b], bobbin, env, index,
                                nocoupling_workspace=fork, normalize_paths=True,
                                strict_hook_contract=True))
                    except Exception as exc:
                        # Preserve the explicit failure and all prior cells;
                        # remaining cells stay missing. No reset/resend follows.
                        row = L0.ArmScore(task["id"], arm, budget, "error",
                                          f"isolated cell refused: {type(exc).__name__}: {exc}")
                        L0.write_jsonl(out, [row])
                        observed_scores.append(row)
                        write_coverage(scratch / "matrix-coverage.json", cells, observed_scores)
                        raise click.ClickException(row.detail) from exc
                    L0.write_jsonl(out, [row])
                    with out.open("a") as written:
                        written.flush()
                        os.fsync(written.fileno())
                    observed_scores.append(row)
                    write_coverage(scratch / "matrix-coverage.json", cells, observed_scores)
                    scores.append(row)
        else:
            scores = L0.run_task(task, ws, arm_list, budget_list, bobbin, env, index)
            L0.write_jsonl(out, scores)
        by = {}
        for s in scores:
            by.setdefault(s.outcome, 0)
            by[s.outcome] += 1
        click.echo(f"{task['id']}: {by}")


@cli.command("l0-report")
@click.argument("scores_path")
@click.option("--budget", default=300, type=int, help="Budget to report (300 is primary).")
def l0_report(scores_path, budget):
    """Preregistered L0 comparison: paired vs `full`, bootstrap CI, Wilcoxon, Holm."""
    from runner import l0 as L0

    rows = [
        json.loads(line)
        for line in Path(scores_path).read_text().splitlines()
        if line.strip() and json.loads(line).get("kind") != "manifest"
    ]
    scores = []
    for d in rows:
        d["chunks"] = [tuple(c) for c in d.get("chunks", [])]
        scores.append(L0.ArmScore(**d))
    click.echo(json.dumps(L0.summarize(scores, budget), indent=2))


@cli.command("j4")
@click.option("--tasks-dir", default="tasks", help="Directory containing task YAML files.")
@click.option("--task", "task_ids", multiple=True, help="Restrict to these task ids (repeatable).")
@click.option("--out", "out_path", required=True, help="JSONL file to append J4 records to.")
@click.option(
    "--workdir",
    required=True,
    help="J4's OWN clone directory. Never an L0 run's: the hook writes into it.",
)
@click.option("--index-timeout", default=1800, type=int, help="Bobbin index timeout in seconds.")
def j4_cmd(tasks_dir, task_ids, out_path, workdir, index_timeout):
    """Eval v2 J4 (PREREGISTRATION amendment A1): judge candidates once and record everything.

    Calls Jev (pinned model) once per candidate, for each task's positive set and its
    wrong-repository pairing, and records the hook's `full` and G(t) outcomes beside them.
    Scoring is `j4-report`, offline. Needs TYPESAFE_API_KEY or TYPESAFE_API_KEY_FILE.
    """
    import subprocess as _sp

    from runner import j4 as J4
    from runner import l0 as L0
    from runner.bobbin_setup import _find_bobbin, setup_bobbin
    from runner.workspace import checkout_parent, clone_repo

    judge = J4.jev_judge()  # fails loud before any work if there is no key
    tasks_path = _resolve_tasks_dir(tasks_dir)
    all_tasks = load_all_tasks(tasks_path)
    all_ids = [t["id"] for t in all_tasks]
    tasks = [t for t in all_tasks if not task_ids or t["id"] in task_ids]

    scratch = Path(workdir)
    scratch.mkdir(parents=True, exist_ok=True)
    env = L0.isolated_env(scratch)
    for k in [k for k in os.environ if k.startswith("BOBBIN_")]:
        del os.environ[k]
    os.environ["XDG_CONFIG_HOME"] = env["XDG_CONFIG_HOME"]
    bobbin = _find_bobbin()

    needed = {t["id"] for t in tasks} | {
        p for t in tasks if (p := J4.negative_partner(t["id"], all_ids))
    }
    by_id = {t["id"]: t for t in all_tasks}
    workspaces: dict[str, Path] = {}
    for tid in sorted(needed):
        task = by_id[tid]
        ws = scratch / tid / task["repo"].replace("/", "--")
        if not (ws / ".bobbin").exists():
            ws = clone_repo(task["repo"], str(scratch / tid))
            checkout_parent(ws, task["commit"])
            setup_bobbin(str(ws), timeout=index_timeout)
        workspaces[tid] = ws

    prereg = Path(__file__).resolve().parent.parent / "v2" / "PREREGISTRATION.md"
    out = Path(out_path)
    out.parent.mkdir(parents=True, exist_ok=True)
    manifest = {
        "kind": "manifest",
        "started_at": datetime.now(timezone.utc).isoformat(),
        "bobbin": _sp.run(
            [bobbin, "--version"], capture_output=True, text=True, env=env, check=False
        ).stdout.strip(),
        "harness_commit": _sp.run(
            ["git", "rev-parse", "HEAD"],
            cwd=Path(__file__).parent,
            capture_output=True,
            text=True,
            check=False,
        ).stdout.strip(),
        "preregistration_sha256": __import__("hashlib").sha256(prereg.read_bytes()).hexdigest(),
        "judge_model": J4.JEV_MODEL,
        "tasks": [t["id"] for t in tasks],
    }
    with out.open("a") as fh:
        fh.write(json.dumps(manifest) + "\n")
    for task in tasks:
        gold = L0.gold_for_commit(workspaces[task["id"]], task["commit"])
        records = J4.run_j4_task(task, workspaces, all_ids, env, bobbin, judge)
        with out.open("a") as fh:
            fh.write(json.dumps({"kind": "gold", "task_id": task["id"], "gold": gold}) + "\n")
            for r in records:
                fh.write(json.dumps(r) + "\n")
        click.echo(
            f"{task['id']}: "
            + ", ".join(f"{r['set']}={r.get('candidate_outcome', r['kind'])}" for r in records)
        )


@cli.command("j4-report")
@click.argument("records_path")
def j4_report(records_path):
    """A1's registered J4 analysis, offline, from recorded judgements. Refuses mixed models."""
    from runner import j4 as J4

    rows = [json.loads(ln) for ln in Path(records_path).read_text().splitlines() if ln.strip()]
    gold = {
        r["task_id"]: {p: [tuple(x) for x in rs] for p, rs in r["gold"].items()}
        for r in rows
        if r.get("kind") == "gold"
    }
    click.echo(json.dumps(J4.report(rows, gold, J4.calibration_ids()), indent=2))


if __name__ == "__main__":
    cli()
