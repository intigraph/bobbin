#!/usr/bin/env python3
"""Record real local CLI output and render a self-contained animated SVG."""
from __future__ import annotations

import argparse
import hashlib
import html
import json
import os
from pathlib import Path
import re
import subprocess
import time
import shlex
import textwrap
import xml.etree.ElementTree as ET

HERE = Path(__file__).resolve().parent
FORBIDDEN = [
    r"/(?:home|Users|root|srv|run)/",
    r"\b[\w.-]+\.(?:lan|svc|internal|local)\b",
    r"\b(?:\d{1,3}\.){3}\d{1,3}\b",
    r"\b(?:[0-9a-fA-F]{1,4}:){2,}[0-9a-fA-F:]+\b",
    r"(?i)\bbearer\s+\S+",
    r"-----BEGIN [A-Z ]*PRIVATE KEY-----",
    r"(?i)(?:token|password|secret)\s*[=:]\s*\S+",
]


def check_text(text: str, *, identifiers: bool = True) -> None:
    """Refuse, never redact a recording into a claimed successful command."""
    if any(re.search(pattern, text) for pattern in FORBIDDEN):
        raise ValueError("recording refused: possible private data")
    if any(ord(char) < 32 and char not in "\n\r\t" for char in text):
        raise ValueError("recording refused: unsupported terminal control")
    # Reject crew work-item prefixes while allowing public CLI flag names.
    if identifiers and re.search(r"\b(?:aegis|na)-[a-z0-9]+(?:\.\d+)*\b", text):
        raise ValueError("recording refused: possible work item identifier")



def render_svg(tape: dict, frames: list[dict], duration: float) -> str:
    width, height = tape["width"], tape["height"]
    title = html.escape(tape["title"])
    pieces = [
        f'<svg xmlns="http://www.w3.org/2000/svg" width="{width}" height="{height}" '
        f'viewBox="0 0 {width} {height}" role="img" aria-labelledby="title desc">',
        f'<title id="title">{title}</title>',
        f'<desc id="desc">{html.escape(tape["description"])} The animation repeats. '
        'The adjacent transcript provides a static alternative.</desc>',
        '<rect width="100%" height="100%" rx="16" fill="#101827"/>',
        f'<path d="M0 54H{width}" stroke="#334155"/>',
        '<circle cx="28" cy="27" r="7" fill="#fb7185"/>',
        '<circle cx="52" cy="27" r="7" fill="#fbbf24"/>',
        '<circle cx="76" cy="27" r="7" fill="#34d399"/>',
        '<text x="110" y="34" fill="#e2e8f0" font-family="monospace" font-size="20">'
        f'{title}</text>',
    ]
    # Last frame is the static fallback when SVG animation is disabled.
    for index, frame in enumerate(frames):
        check_text("\n".join(frame["lines"]))
        last = index == len(frames) - 1
        start = frame["at"] / duration
        end = (frames[index + 1]["at"] / duration) if not last else 1
        values, keys = ["hidden"], [0.0]
        if start == 0:
            values[0] = "visible"
        else:
            values.append("visible")
            keys.append(start)
        if not last:
            values.append("hidden")
            keys.append(end)
        values.append(values[-1])
        keys.append(1.0)
        pieces.append(f'<g id="frame{index}" visibility="{"visible" if last else "hidden"}" '
                      f'font-family="DejaVu Sans Mono, Menlo, monospace" font-size="{tape["font_size"]}">')
        pieces.append(f'<animate attributeName="visibility" calcMode="discrete" '
                      f'values="{";".join(values)}" keyTimes="{";".join(f"{k:.6f}" for k in keys)}" '
                      f'dur="{duration:.3f}s" repeatCount="indefinite"/>')
        for row, line in enumerate(frame["lines"]):
            color = "#6ee7b7" if line.startswith("$ ") else "#f1f5f9"
            pieces.append(f'<text x="28" y="{94 + row * tape["line_height"]}" '
                          f'fill="{color}" xml:space="preserve">{html.escape(line)}</text>')
        pieces.append('</g>')
    pieces.append('</svg>')
    return "\n".join(pieces) + "\n"


def digest(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("tape", type=Path)
    parser.add_argument("--workspace", type=Path, required=True,
                        help="prepared PRIVATE sandbox; commands may write here")
    parser.add_argument("--tool", action="append", default=[], metavar="NAME=PATH")
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    tape = json.loads(args.tape.read_text())
    if tape["font_size"] != 24 or not 1 <= len(tape["steps"]) <= 12:
        raise ValueError("use the shared 24px theme and at most twelve steps")
    tools = dict(value.split("=", 1) for value in args.tool)
    binaries = {name: Path(path).resolve(strict=True) for name, path in tools.items()}
    root = args.workspace.resolve(strict=True)
    if not (root / ".terminal-demo-sandbox").is_file():
        raise ValueError("workspace needs an explicit .terminal-demo-sandbox marker")
    home = root / ".demo-home"
    home.mkdir(exist_ok=True)
    # Never inherit endpoints, tokens, user configuration or tool search paths.
    env = {"PATH": os.pathsep.join(dict.fromkeys(
               [str(p.parent) for p in binaries.values()] + [os.defpath])),
           "HOME": str(home), "XDG_CONFIG_HOME": str(home / ".config"),
           "XDG_CACHE_HOME": str(home / ".cache"),
           "XDG_STATE_HOME": str(home / ".state"), "TMPDIR": str(root),
           "LANG": "C.UTF-8", "TERM": "dumb", "NO_COLOR": "1", "RUST_LOG": "off",
           "BOBBIN_GPU": "0"}
    binary_hashes = {name: digest(path) for name, path in binaries.items()}
    version_args = {name: tape.get("version_args", {}).get(name, ["--version"])
                    for name in binaries}
    versions = {name: subprocess.check_output([str(path), *version_args[name]], env=env,
                text=True, timeout=10).strip() for name, path in binaries.items()}
    frames, events, runs, transcript = [], [], [], []
    elapsed = 0.0
    for step in tape["steps"]:
        name = step["tool"]
        command = "$ " + shlex.join([name, *step["args"]])
        stdin = None
        if "stdin" in step:
            input_path = (root / step["stdin"]).resolve(strict=True)
            input_path.relative_to(root)
            stdin = input_path.read_text()
            command += " < " + shlex.quote(step["stdin"])
        check_text(command)
        start = time.monotonic()
        result = subprocess.run([str(binaries[name]), *step["args"]], cwd=root,
                                env=env, input=stdin, capture_output=True,
                                text=True, timeout=step.get("timeout", 30))
        seconds = time.monotonic() - start
        check_text(result.stdout + result.stderr)
        if result.returncode != step.get("exit", 0):
            raise RuntimeError("unexpected command exit; no render published")
        if result.stderr and not step.get("allow_stderr", False):
            raise RuntimeError("unexpected stderr; no render published")
        body = result.stdout + result.stderr
        for required in step.get("requires", []):
            if required not in body:
                raise RuntimeError("required output absent; no render published")
        transcript.extend([command, body.rstrip(), f"[exit {result.returncode}]"])
        # Page complete output, never ellipsize or silently discard a line.
        lines = []
        for line in (command + "\n" + body.rstrip()).splitlines():
            lines.extend(textwrap.wrap(line, width=65, replace_whitespace=False,
                                       drop_whitespace=False) or [""])
        pages = [lines[i:i + 14] for i in range(0, len(lines), 14)]
        for page in pages:
            check_text("\n".join(page))
            frames.append({"at": elapsed, "lines": page})
            events.append([elapsed, "o", "\x1b[2J\x1b[H" + "\r\n".join(page) + "\r\n"])
            elapsed += step.get("hold", 5) / len(pages)
        runs.append({"tool": name, "args": step["args"], "exit": result.returncode,
                     "seconds": round(seconds, 6), "stdout": result.stdout,
                     "stderr": result.stderr})
    if not 10 <= elapsed <= 30:
        raise ValueError("reading timeline must be 10 to 30 seconds")
    svg = render_svg(tape, frames, elapsed)
    ET.fromstring(svg)
    check_text(svg, identifiers=False)
    cast = {"version": 2, "width": 65, "height": 14, "title": tape["title"],
            "env": {"TERM": "dumb"}}
    artifacts = {"demo.svg": svg, "transcript.txt": "\n".join(transcript) + "\n",
                 "demo.cast": json.dumps(cast) + "\n" + "\n".join(
                     json.dumps(event) for event in events) + "\n"}
    for name, body in artifacts.items():
        check_text(body, identifiers=name != "demo.svg")
    inputs = {"render.py": digest(Path(__file__)), "tape": digest(args.tape)}
    for name in tape.get("inputs", []):
        path = (root / name).resolve(strict=True)
        path.relative_to(root)
        inputs[name] = digest(path)
    if binary_hashes != {name: digest(path) for name, path in binaries.items()}:
        raise RuntimeError("tool bytes changed during capture; no render published")
    manifest = {"theme": "stack-terminal-24", "versions": versions,
                "binary_sha256": binary_hashes, "version_args": version_args,
                "inputs_sha256": inputs, "duration_seconds": elapsed,
                "timeline": "deliberate reading holds; execution times recorded separately",
                "frames_checked": len(frames), "commands": runs,
                "sha256": {n: hashlib.sha256(b.encode()).hexdigest()
                           for n, b in artifacts.items()}}
    args.output.mkdir(parents=True, exist_ok=True)
    for name, body in artifacts.items():
        (args.output / name).write_text(body)
    (args.output / "manifest.json").write_text(json.dumps(manifest, indent=2) + "\n")
    print(json.dumps({"frames_checked": len(frames), "duration": elapsed,
                      "svg_sha256": manifest["sha256"]["demo.svg"]}))


if __name__ == "__main__":
    main()
