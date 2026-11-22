#!/usr/bin/env python3
"""Execute the release pack step with a real versioned shared-library fixture."""

import os
from pathlib import Path
import re
import shutil
import subprocess
import tempfile


root = Path(__file__).resolve().parent.parent
workflow = (root / ".github/workflows/release.yml").read_text()
step = workflow.split("      - name: Build repository index pack\n", 1)[1]
step = step.split("      - name:", 1)[0]
script = step.split("        run: |\n", 1)[1]
script = "\n".join(line[10:] for line in script.splitlines() if line.strip())
assert shutil.which("cc"), "native shared-library control requires a C compiler"

with tempfile.TemporaryDirectory() as tmp:
    home = Path(tmp)
    lib = home / "ort/lib/libonnxruntime.so.1.24.1"
    lib.parent.mkdir(parents=True)
    source = home / "runtime.c"
    source.write_text("int fixture_runtime(void) { return 7; }\n")
    subprocess.run(["cc", "-shared", "-fPIC", str(source), "-o", str(lib)], check=True)
    cli = home / "target/fixture/release/bobbin"
    cli.parent.mkdir(parents=True)
    cli.write_text('''#!/usr/bin/env python3
import ctypes, os, pathlib, sys
with open(os.environ["PACK_CALLS"], "a") as log:
    log.write(" ".join(sys.argv[1:]) + "\\n")
if sys.argv[1] == "index":
    runtime = ctypes.CDLL(os.environ.get("ORT_DYLIB_PATH", "libonnxruntime.so"))
    assert runtime.fixture_runtime() == 7
elif sys.argv[1:3] == ["pack", "export"]:
    pathlib.Path(sys.argv[sys.argv.index("--output") + 1]).write_text("fixture pack")
elif sys.argv[1:3] == ["pack", "verify"]:
    assert pathlib.Path(sys.argv[3]).is_file()
''')
    cli.chmod(0o755)
    script = script.replace("${{ env.VERSION }}", "v-fixture")
    script = script.replace("${{ matrix.target }}", "fixture")
    script = script.replace("${{ matrix.ort_lib }}", lib.name)
    script = script.replace("/tmp/ort-extracted", str(home / "ort"))
    assert "${{" not in script
    env = {k: v for k, v in os.environ.items() if k not in {"ORT_DYLIB_PATH", "LD_LIBRARY_PATH"}}
    calls = home / "calls"
    env.update(GITHUB_WORKSPACE=str(home), PACK_CALLS=str(calls), TMPDIR=str(home))

    def run(body):
        calls.unlink(missing_ok=True)
        result = subprocess.run(["bash", "-euo", "pipefail", "-c", body], cwd=home,
                                env=env, capture_output=True, text=True, timeout=30)
        return result, calls.read_text() if calls.exists() else ""

    result, observed = run(script)
    assert result.returncode == 0, result.stdout + result.stderr
    assert "index " in observed and "pack verify " in observed, observed
    print("PASS: versioned library loads through the actual pack workflow")

    # Keep LD_LIBRARY_PATH, but remove the explicit file binding. No unversioned
    # soname exists: this is the exact wheel-layout failure from release 0.37.0.
    mutant = re.sub(r"^\s*export ORT_DYLIB_PATH=.*$", "", script, flags=re.M)
    mutant = re.sub(r'^\s*test -r "\$ORT_DYLIB_PATH"$', "", mutant, flags=re.M)
    result, observed = run(mutant)
    assert result.returncode != 0 and "libonnxruntime.so" in result.stderr
    assert "pack export " not in observed, observed
    print("PASS: directory-only lookup fails before pack export")

    lib.unlink()
    result, observed = run(script)
    assert result.returncode != 0 and not observed, result.stdout + result.stderr
    print("PASS: missing pinned library refuses before any Bobbin invocation")
