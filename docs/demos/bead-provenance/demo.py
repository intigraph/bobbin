# /// script
# requires-python = ">=3.11"
# dependencies = []
# ///
"""Compare two real MCP binaries against one retained loopback-store fixture.

# arming: agent-invoked PR demonstration; no production writes or tracker reads.
"""
import argparse
import http.server
import json
import os
from pathlib import Path
import select
import shutil
import subprocess
import tempfile
import threading
import time

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("--before", required=True)
parser.add_argument("--after", required=True)
args = parser.parse_args()

class Store(http.server.BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass

    def do_GET(self):
        if not self.path.startswith("/beads?"):
            self.send_response(404)
            self.end_headers()
            return
        body = json.dumps({"query": "retained", "count": 1, "results": [{
            "bead_id": "example-1", "title": "Retained issue", "priority": "P2",
            "status": "open", "assignee": "former", "issue_type": "task",
            "owner": "", "rig": "example", "relevance_score": 0.5,
            "match_type": "hybrid",
        }]}).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

def rpc(process, request):
    process.stdin.write(json.dumps(request) + "\n")
    process.stdin.flush()
    deadline = time.monotonic() + 20
    while time.monotonic() < deadline:
        ready, _, _ = select.select([process.stdout], [], [], max(0, deadline - time.monotonic()))
        if not ready:
            break
        line = process.stdout.readline()
        if not line:
            raise RuntimeError("MCP process exited before response")
        response = json.loads(line)
        if response.get("id") == request["id"]:
            if "error" in response:
                raise RuntimeError(response["error"])
            return response["result"]
    raise RuntimeError("MCP response timed out")

server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Store)
threading.Thread(target=server.serve_forever, daemon=True).start()
try:
    for label, binary in [("before", args.before), ("after", args.after)]:
        executable = str(Path(shutil.which(binary) or binary).resolve())
        with tempfile.TemporaryDirectory() as root:
            env = {k: v for k, v in os.environ.items() if not k.startswith(("QUIPU_", "BOBBIN_"))}
            env.update(HOME=root, XDG_CONFIG_HOME=root, XDG_STATE_HOME=root, XDG_CACHE_HOME=root)
            process = subprocess.Popen([executable, "--server", f"http://127.0.0.1:{server.server_port}", "serve", root], env=env, cwd=root, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
            try:
                rpc(process, {"jsonrpc":"2.0", "id":1, "method":"initialize", "params":{
                    "protocolVersion":"2024-11-05", "capabilities":{}, "clientInfo":{"name":"bead-provenance-demo", "version":"1"}}})
                process.stdin.write(json.dumps({"jsonrpc":"2.0", "method":"notifications/initialized"}) + "\n")
                process.stdin.flush()
                result = rpc(process, {"jsonrpc":"2.0", "id":2, "method":"tools/call", "params":{
                    "name":"search_beads", "arguments":{"query":"retained", "limit":1, "enrich":True, "compact":True}}})
                value = json.loads(result["content"][0]["text"])
                row = value["results"][0]
                assert row["assignee"] == "former" and row["status"] == "open"
                projection = {"count":value["count"], "assignee":row["assignee"], "status":row["status"], "metadata_provenance":row.get("metadata_provenance"), "warnings":value.get("warnings", [])}
                if label == "before":
                    assert projection["metadata_provenance"] is None
                    assert not projection["warnings"]
                else:
                    assert projection["metadata_provenance"]["source"] == "unreported"
                    assert projection["metadata_provenance"]["current_board_verified"] is False
                    assert projection["metadata_provenance"]["as_of"] is None
                    assert projection["warnings"]
                print(label + ": " + json.dumps(projection, indent=2), flush=True)
            finally:
                process.terminate()
                try:
                    process.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait(timeout=5)
finally:
    server.shutdown()
print("PASS: retained fields remain visible; candidate warns they are unverified.")
