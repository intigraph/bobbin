#!/usr/bin/env python3
"""Prepare a disposable index and model cache for the terminal recorder."""
import argparse
import os
from pathlib import Path
import shutil
import subprocess


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("workspace", type=Path)
    parser.add_argument("--tool", type=Path, required=True)
    args = parser.parse_args()
    tool = args.tool.resolve(strict=True)
    root = args.workspace.resolve()
    root.mkdir()  # Refuse an existing workspace rather than reuse its state.
    home = root / ".demo-home"
    home.mkdir()
    shutil.copyfile(Path(__file__).parent / "fixture/totals.py", root / "totals.py")
    (root / ".terminal-demo-sandbox").write_text("disposable terminal fixture\n")
    env = {
        "PATH": str(tool.parent) + os.pathsep + os.defpath,
        "HOME": str(home),
        "XDG_CONFIG_HOME": str(home / ".config"),
        "XDG_CACHE_HOME": str(home / ".cache"),
        "XDG_STATE_HOME": str(home / ".state"),
        "TMPDIR": str(root),
        "LANG": "C.UTF-8",
        "TERM": "dumb",
        "NO_COLOR": "1",
        "RUST_LOG": "off",
        "BOBBIN_GPU": "0",
    }
    for command in [
        ["init", ".", "--quiet"],
        ["index", ".", "--repo", "totals-example", "--no-quipu-publish", "--skip-calibrate", "--quiet"],
    ]:
        subprocess.run([str(tool), *command], cwd=root, env=env, check=True)


if __name__ == "__main__":
    main()
