"""Compare two CLI builds using disposable local repositories, without an index."""

import hashlib
import json
import os
from pathlib import Path
import subprocess
import tempfile


def demonstrate(label, binary):
    binary = Path(binary).resolve()
    print(f"{label} binary SHA256: {hashlib.sha256(binary.read_bytes()).hexdigest()}")
    with tempfile.TemporaryDirectory(prefix="breadcrumb-demo-") as directory:
        root = Path(directory)
        home = root / "home"
        home.mkdir()
        env = {
            key: value
            for key, value in os.environ.items()
            if not key.startswith(("BOBBIN_", "SHANTY_"))
        }
        env.update(
            HOME=str(home),
            XDG_CONFIG_HOME=str(home / "config"),
            XDG_DATA_HOME=str(home / "data"),
            BOBBIN_GPU="0",
        )

        def run(args, payload=None):
            result = subprocess.run(
                [str(binary), *args],
                cwd=root,
                env=env,
                input=json.dumps(payload) if payload is not None else None,
                capture_output=True,
                text=True,
                timeout=10,
            )
            assert result.returncode == 0, "CLI call failed"
            return result.stdout

        run(["init", str(root)])
        (root / "auth.rs").write_text("fn refresh_token() {}\n")
        run([
            "mark", "auth-refresh", "refresh authentication", "Refresh flow",
            "--pin", "auth.rs", "--on", "refresh_token", "--path", str(root),
        ])
        store = root / ".bobbin/breadcrumbs.json"
        before = store.read_bytes()

        def hook(name, session, **fields):
            return run(
                ["hook", name, "--budget", "20"],
                {"cwd": str(root), "session_id": session, **fields},
            )

        startup = json.loads(run(["hook", "prime-context"], {"cwd": str(root)}))
        discovery = startup["hookSpecificOutput"]["additionalContext"]
        assert "auth-refresh" in discovery
        print("SessionStart discovery: auth-refresh (already present in both builds)")
        prompt = hook("inject-context", "shared", prompt="Explain REFRESH_TOKEN implementation")
        print("Prompt keyword:")
        print(prompt.rstrip() or "(quiet)")
        duplicate = hook(
            "post-tool-use", "shared", tool_name="UnknownControl",
            tool_input={"message": "refresh_token"},
        )
        assert "Breadcrumb: auth-refresh" not in duplicate
        print("Same-session tool keyword: quiet (no duplicate)")
        tool = hook(
            "post-tool-use", "new", tool_name="UnknownControl",
            tool_input={"message": "ReFrEsH_ToKeN"},
        )
        context = json.loads(tool)["hookSpecificOutput"]["additionalContext"] if tool else ""
        print("New-session tool keyword:")
        print(context.rstrip() or "(quiet)")
        expected = label == "AFTER"
        assert ("Breadcrumb: auth-refresh" in prompt) == expected
        assert ("Breadcrumb: auth-refresh" in context) == expected
        assert len(prompt.splitlines()) <= 6 and len(context.splitlines()) <= 6
        assert store.read_bytes() == before
        print("Budget: at most 6 of 20 lines; breadcrumb store unchanged\n")


if __name__ == "__main__":
    demonstrate("BEFORE", os.environ["BOBBIN_DEMO_BEFORE"])
    demonstrate("AFTER", os.environ["BOBBIN_DEMO_AFTER"])
