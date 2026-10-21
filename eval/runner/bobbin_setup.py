"""Bobbin init + index on a workspace for with-bobbin eval runs."""

from __future__ import annotations

import json
import logging
import os
import signal
import re
import shutil
import subprocess
import time
from pathlib import Path
from typing import Any, Callable

logger = logging.getLogger(__name__)

# Mapping from override key to (TOML section, TOML key, value type).
# blame_bridging maps to [hooks] show_docs as a proxy — full support requires
# a Rust-side config toggle (see docs/tasks/).
OVERRIDE_MAP: dict[str, tuple[str, str, type]] = {
    "git.coupling_enabled": ("git", "coupling_enabled", bool),
    "semantic_weight": ("search", "semantic_weight", float),
    "coupling_depth": ("git", "coupling_depth", int),
    "gate_threshold": ("hooks", "gate_threshold", float),
    "doc_demotion": ("search", "doc_demotion", float),
    "recency_weight": ("search", "recency_weight", float),
    "blame_bridging": ("hooks", "show_docs", bool),
    "format_mode": ("hooks", "format_mode", str),
    "neighbor_budget_pct": ("context", "neighbor_budget_pct", float),
}

ALLOWED_OVERRIDE_KEYS = frozenset(OVERRIDE_MAP.keys())

_WORKSPACE_CLAUDE_MD = """\
# Project Tools

This project is indexed by **bobbin**, a semantic code search engine.

## How to navigate this codebase

Use bobbin commands instead of manual grep/find — they search by meaning, not just text:

```bash
bobbin search "error handling in auth"    # semantic + keyword hybrid search
bobbin context "fix PYI034 rule"          # focused context bundle for a task
bobbin related src/rules/some_rule.rs     # files that change together
bobbin refs find SomeFunction              # find definitions and usages
bobbin grep "pattern"                     # regex search across all files
```

## Workflow

1. **Start with `bobbin search`** to find relevant code for your task
2. **Use `bobbin related`** on key files to discover test files and dependencies
3. **Use `bobbin refs find`** to trace symbol usage across the codebase
4. Read the files bobbin identifies, then make targeted changes

Bobbin context is also injected automatically when you submit prompts and after
file edits — check the system messages for relevant code snippets.
"""


class BobbinSetupError(Exception):
    """Raised when bobbin init or index fails."""


def _find_bobbin() -> str:
    """Find the bobbin binary, preferring PATH then common install locations."""
    found = shutil.which("bobbin")
    if found:
        return found
    cargo_bin = Path.home() / ".cargo" / "bin" / "bobbin"
    if cargo_bin.exists():
        return str(cargo_bin)
    raise BobbinSetupError("bobbin binary not found. Install with: cargo install bobbin-ai")


def _parse_profile(output: str) -> dict[str, Any] | None:
    """Extract profiling data from ``bobbin index -v`` output.

    Parses the ``Profile:`` block emitted when verbose mode is enabled.
    Returns a dict of phase timings (in ms) or None if no profile found.
    """
    profile: dict[str, Any] = {}
    in_profile = False
    for line in output.splitlines():
        stripped = line.strip()
        if stripped.startswith("Profile:"):
            in_profile = True
            continue
        if not in_profile:
            continue
        # TOTAL line: "  TOTAL:           310ms"
        m = re.match(r"^TOTAL:\s+(\d+)ms", stripped)
        if m:
            profile["total_ms"] = int(m.group(1))
            continue
        # embed throughput line: "  embed throughput: 123.4 chunks/s"
        m = re.match(r"^embed throughput:\s+([\d.]+)\s+chunks/s", stripped)
        if m:
            profile["embed_throughput_chunks_per_sec"] = float(m.group(1))
            continue
        # Each line looks like: "  file I/O:       123ms"
        # or "  embed:          456ms  (100 chunks in 2 batches)"
        # or sub-phase: "    tokenize:     123ms"
        m = re.match(r"^(\S[^:]+):\s+(\d+)ms", stripped)
        if m:
            key = m.group(1).strip().replace(" ", "_").lower()
            profile[key] = int(m.group(2))
            continue
        # Non-matching line after Profile block → end of profile
        if in_profile and stripped:
            break
    return profile if profile else None


def setup_bobbin(
    workspace: str,
    *,
    timeout: int = 1800,
    config_overrides: dict[str, str] | None = None,
    initialize: bool = True,
    init_timeout: int = 30,
    before_index: Callable[[Path], None] | None = None,
) -> dict[str, Any]:
    """Run bobbin init and index on the given workspace.

    Parameters
    ----------
    workspace:
        Path to the git working copy where bobbin should be initialized.
    timeout:
        Max seconds for the index step (init is fast, index can be slow).
    config_overrides:
        Optional dict of ``{key: raw_value}`` overrides to apply to
        ``.bobbin/config.toml`` after init but before indexing.
    init_timeout:
        Max seconds for ``bobbin init``. A wrapper that verifies a pinned GPU
        runtime before exec spends this budget too; the pilot passes its task
        timeout because that verification exceeded 30 s under host load
        (aegis-bgk9ho).

    Returns a metadata dict with index timing and bobbin status info.

    Raises :class:`BobbinSetupError` if init or index fails.
    """
    ws = Path(workspace)
    bobbin = _find_bobbin()

    if initialize:
        logger.info("Initializing bobbin in %s", ws)
        try:
            subprocess.run(
                [bobbin, "init"],
                cwd=ws,
                check=True,
                capture_output=True,
                text=True,
                timeout=init_timeout,
            )
        except subprocess.CalledProcessError as exc:
            raise BobbinSetupError(f"bobbin init failed: {exc.stderr.strip()}") from exc
        except subprocess.TimeoutExpired as exc:
            raise BobbinSetupError(
                f"bobbin init timed out after {init_timeout}s: {(exc.stderr or '')[-2000:]!r}"
            ) from exc

    # Apply config overrides between init and index so that index-time
    # parameters (coupling_depth) take effect.
    if config_overrides:
        apply_config_overrides(workspace, config_overrides)

    # Write workspace CLAUDE.md for agent guidance.
    claude_dir = ws / ".claude"
    claude_dir.mkdir(exist_ok=True)
    claude_md = claude_dir / "CLAUDE.md"
    claude_md.write_text(_WORKSPACE_CLAUDE_MD, encoding="utf-8")
    logger.info("Wrote CLAUDE.md to %s", claude_md)

    if before_index is not None:
        before_index(ws)
    logger.info("Indexing workspace %s", ws)
    t0 = time.monotonic()
    try:
        proc = subprocess.Popen(
            [bobbin, "index", "--verbose"],
            cwd=ws,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            start_new_session=(os.name == "posix"),
        )
        # Bound execution while draining BOTH pipes. Reading stderr to EOF
        # before wait(timeout) let a running index exceed its whole budget.
        stdout_text, stderr_text = proc.communicate(timeout=timeout)
        stderr_lines = stderr_text.splitlines()
        for line in stderr_lines:
            if line.startswith("progress:"):
                logger.info("bobbin index %s", line)
        if proc.returncode != 0:
            raise subprocess.CalledProcessError(
                proc.returncode, proc.args,
                output=stdout_text, stderr="\n".join(stderr_lines),
            )
    except subprocess.CalledProcessError as exc:
        raise BobbinSetupError(f"bobbin index failed: {exc.stderr.strip()}") from exc
    except subprocess.TimeoutExpired:
        if os.name == "posix":
            # This PID is the session/group leader created above, never a
            # shared shell or crew process group. Reap Git/index children too.
            try:
                os.killpg(proc.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
        else:
            proc.kill()
        proc.communicate()
        raise BobbinSetupError(f"bobbin index timed out after {timeout}s")
    index_duration = time.monotonic() - t0

    # Capture bobbin status for metadata.
    metadata: dict[str, Any] = {"index_duration_seconds": round(index_duration, 2)}

    # Parse profiling data from verbose output.
    profile = _parse_profile(stdout_text)
    if profile:
        metadata["profile"] = profile
    try:
        status_result = subprocess.run(
            [bobbin, "status", "--json"],
            cwd=ws,
            capture_output=True,
            text=True,
            timeout=30,
        )
        if status_result.returncode == 0:
            status_data = json.loads(status_result.stdout)
            metadata["total_files"] = status_data.get("total_files")
            metadata["total_chunks"] = status_data.get("total_chunks")
            metadata["total_embeddings"] = status_data.get("total_embeddings")
            metadata["languages"] = status_data.get("languages", [])
    except (subprocess.CalledProcessError, subprocess.TimeoutExpired, json.JSONDecodeError) as exc:
        logger.warning("Could not capture bobbin status: %s", exc)

    metadata["gpu_acceleration_proven"] = "ONNX session using CUDA GPU acceleration" in "\n".join(stderr_lines)
    logger.info("Bobbin setup complete for %s (indexed in %.1fs)", ws, index_duration)
    return metadata


def parse_config_override(spec: str) -> tuple[str, str]:
    """Parse a ``key=value`` override string.

    Returns ``(key, raw_value)`` after validating the key is in
    :data:`ALLOWED_OVERRIDE_KEYS`.

    Raises :class:`ValueError` on unknown keys or malformed specs.
    """
    if "=" not in spec:
        raise ValueError(
            f"Invalid override format: {spec!r}  (expected key=value)"
        )
    key, _, raw_value = spec.partition("=")
    key = key.strip()
    raw_value = raw_value.strip()
    if key not in ALLOWED_OVERRIDE_KEYS:
        raise ValueError(
            f"Unknown override key: {key!r}  "
            f"(allowed: {', '.join(sorted(ALLOWED_OVERRIDE_KEYS))})"
        )
    return key, raw_value


def apply_config_overrides(
    workspace: str, overrides: dict[str, str],
) -> None:
    """Modify ``.bobbin/config.toml`` in *workspace* to apply *overrides*.

    Each entry in *overrides* maps an override key (e.g. ``"semantic_weight"``)
    to a raw string value (e.g. ``"0.0"``).  The value is cast to the
    appropriate type and written into the correct TOML section.

    Must be called **after** ``bobbin init`` (which creates the config file)
    and **before** ``bobbin index`` if the override affects indexing
    (e.g. ``coupling_depth``).
    """
    config_path = Path(workspace) / ".bobbin" / "config.toml"
    if not config_path.exists():
        raise BobbinSetupError(
            f"Config not found at {config_path} — run bobbin init first"
        )

    content = config_path.read_text(encoding="utf-8")

    for key, raw_value in overrides.items():
        section, toml_key, value_type = OVERRIDE_MAP[key]
        typed = _cast_value(key, raw_value, value_type)
        content = _set_toml_value(content, section, toml_key, typed)
        logger.info("Config override: [%s] %s = %r", section, toml_key, typed)

    config_path.write_text(content, encoding="utf-8")


def generate_override_settings(
    base_settings_path: str,
    overrides: dict[str, str],
    output_path: str,
) -> str:
    """Create a modified settings JSON with hook-level overrides applied.

    For overrides that affect hook commands (``gate_threshold``), rewrites
    the hook command line in the settings file.  Returns the path to the
    generated settings file.
    """
    base = Path(base_settings_path)
    if not base.exists():
        raise BobbinSetupError(f"Base settings not found: {base}")

    settings = json.loads(base.read_text(encoding="utf-8"))

    # gate_threshold: rewrite --gate-threshold in the hook command
    if "gate_threshold" in overrides:
        gt_val = overrides["gate_threshold"]
        _rewrite_hook_flag(settings, "--gate-threshold", gt_val)

    # blame_bridging: rewrite --show-docs in the hook command
    if "blame_bridging" in overrides:
        show_docs = "true" if overrides["blame_bridging"].lower() in ("true", "1", "yes") else "false"
        _rewrite_hook_flag(settings, "--show-docs", show_docs)

    # format_mode: rewrite --format-mode in the hook command
    if "format_mode" in overrides:
        _rewrite_hook_flag(settings, "--format-mode", overrides["format_mode"])

    out = Path(output_path)
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text(json.dumps(settings, indent=2), encoding="utf-8")
    return str(out)


def _rewrite_hook_flag(settings: dict, flag: str, value: str) -> None:
    """Replace or append *flag* in all hook commands in *settings*."""
    hooks = settings.get("hooks", {})
    for _event, hook_groups in hooks.items():
        if not isinstance(hook_groups, list):
            continue
        for group in hook_groups:
            for hook in group.get("hooks", []):
                cmd = hook.get("command", "")
                if "bobbin" not in cmd:
                    continue
                # Replace existing flag value or append
                pattern = re.compile(rf"{re.escape(flag)}\s+\S+")
                if pattern.search(cmd):
                    hook["command"] = pattern.sub(f"{flag} {value}", cmd)
                else:
                    hook["command"] = f"{cmd} {flag} {value}"


def _cast_value(key: str, raw: str, vtype: type) -> int | float | bool | str:
    """Cast a raw string value to the expected type."""
    try:
        if vtype is str:
            return raw
        if vtype is bool:
            return raw.lower() in ("true", "1", "yes")
        if vtype is int:
            return int(raw)
        return float(raw)
    except (ValueError, TypeError) as exc:
        raise ValueError(
            f"Cannot cast override {key}={raw!r} to {vtype.__name__}"
        ) from exc


def _set_toml_value(
    content: str, section: str, key: str, value: int | float | bool,
) -> str:
    """Replace or insert a value in a TOML string.

    Uses simple line-based parsing — sufficient for bobbin's flat config
    structure.  If the key exists under ``[section]``, its value is replaced.
    If the section exists but the key doesn't, the key is appended.
    If the section doesn't exist, both are appended.
    """
    toml_val = _format_toml_value(value)
    lines = content.splitlines(keepends=True)
    section_header = f"[{section}]"

    in_section = False
    section_start = -1
    section_end = len(lines)
    key_line_idx = -1

    for i, line in enumerate(lines):
        stripped = line.strip()
        if stripped == section_header:
            in_section = True
            section_start = i
            continue
        if in_section and stripped.startswith("[") and stripped.endswith("]"):
            section_end = i
            break
        if in_section and stripped.startswith(f"{key} ") or (
            in_section and stripped.startswith(f"{key}=")
        ):
            key_line_idx = i

    if key_line_idx >= 0:
        # Replace existing key
        lines[key_line_idx] = f"{key} = {toml_val}\n"
    elif section_start >= 0:
        # Section exists but key missing — insert after last key in section
        lines.insert(section_end, f"{key} = {toml_val}\n")
    else:
        # Section missing — append at end
        lines.append(f"\n{section_header}\n{key} = {toml_val}\n")

    return "".join(lines)


def _format_toml_value(value: int | float | bool | str) -> str:
    """Format a Python value as a TOML literal."""
    if isinstance(value, str):
        return f'"{value}"'
    if isinstance(value, bool):
        return "true" if value else "false"
    if isinstance(value, int):
        return str(value)
    # Float: ensure at least one decimal
    s = f"{value:.6g}"
    if "." not in s and "e" not in s.lower():
        s += ".0"
    return s
