"""Build one outward stack share using the existing Quipu/Bobbin boundary.

Source and tooling are separate fresh, tag-verified checkouts. Index configuration,
live policy, databases and receiver probes stay in the private working directory.
Only the verified share directory leaves this function.
"""

import hashlib
import io
import json
import os
import re
import shutil
import sqlite3
import subprocess
import sys
import tarfile
from pathlib import Path


def isolated_environment(private):
    """Native workers must not inherit a host endpoint or credential catalogue."""
    private = Path(private)
    env = dict(os.environ)
    for key in list(env):
        if key in {
            "BOBBIN_SERVER",
            "QUIPU_SERVER",
            "QUIPU_DB",
            "QUIPU_AUTH_TOKEN",
            "QUIPU_AUTH_TOKEN_FILE",
            "GH_TOKEN",
            "GITHUB_TOKEN",
        } or key.startswith("BOBBIN_QUIPU_"):
            env.pop(key)
    for key, name in (
        ("HOME", "native-home"),
        ("XDG_CONFIG_HOME", "native-config"),
        ("XDG_DATA_HOME", "native-data"),
        ("XDG_CACHE_HOME", "native-cache"),
    ):
        directory = private / name
        directory.mkdir(mode=0o700, parents=True, exist_ok=True)
        env[key] = str(directory)
    return env


def command(args, *, env=None, cwd=None):
    if env is None and cwd is not None:
        env = isolated_environment(cwd)
    return subprocess.run(
        [str(v) for v in args],
        check=True,
        capture_output=True,
        text=True,
        env=env,
        cwd=cwd,
    ).stdout


def local_mcp(binary, database, calls):
    """Short-lived native process over a private DB; no HTTP or live graph."""
    import selectors

    requests = [
        {
            "jsonrpc": "2.0",
            "id": 0,
            "method": "initialize",
            "params": {
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": {"name": "repository-pendant-build", "version": "1"},
            },
        },
        {"jsonrpc": "2.0", "method": "notifications/initialized"},
    ]
    requests.extend(
        {
            "jsonrpc": "2.0",
            "id": number,
            "method": "tools/call",
            "params": {"name": name, "arguments": arguments},
        }
        for number, (name, arguments) in enumerate(calls, 1)
    )
    values = []
    with subprocess.Popen(
        [str(binary), "mcp", "--db", str(database)],
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        cwd=Path(database).parent,
        env=isolated_environment(Path(database).parent),
    ) as process:
        selector = selectors.DefaultSelector()
        selector.register(process.stdout, selectors.EVENT_READ)
        try:
            for request in requests:
                process.stdin.write(json.dumps(request) + "\n")
                process.stdin.flush()
                if "id" not in request:
                    continue
                if not selector.select(timeout=30):
                    raise ValueError("native MCP response timeout")
                response = json.loads(process.stdout.readline())
                if (
                    response.get("id") != request["id"]
                    or "result" not in response
                    or "error" in response
                    or response["result"].get("isError", False)
                ):
                    raise ValueError(
                        "native MCP refused operation: " + json.dumps(response)
                    )
                if request["id"]:
                    content = response["result"].get("content", [])
                    if len(content) != 1 or content[0].get("type") != "text":
                        raise ValueError("unexpected native MCP result envelope")
                    values.append(json.loads(content[0]["text"]))
        finally:
            selector.close()
            process.stdin.close()
            try:
                process.wait(timeout=10)
            except subprocess.TimeoutExpired:
                process.terminate()
                process.wait(timeout=10)
        if process.returncode:
            raise ValueError("native MCP process failed")
    return values


def register_queries(binary, database, definitions):
    if not isinstance(definitions, list) or not definitions:
        raise ValueError("explicit repository stored query definitions required")
    for definition in definitions:
        if (
            not isinstance(definition, dict)
            or definition.get("action", "load") != "load"
        ):
            raise ValueError("stored query must be an explicit load definition")
    results = local_mcp(
        binary,
        database,
        [("quipu_queries", {**d, "action": "load"}) for d in definitions],
    )
    if [result.get("loaded") for result in results] != [d["name"] for d in definitions]:
        raise ValueError("native registry did not return the exact loaded query names")


def indexed_labels(binary, database, subjects):
    if any(
        not re.fullmatch(r'https?://[^\s<>"{}|\\^`]+', subject) for subject in subjects
    ):
        raise ValueError("invalid indexed subject IRI")
    query = (
        "SELECT ?s ?label WHERE { VALUES ?s { "
        + " ".join("<" + s + ">" for s in subjects)
        + "} ?s <http://www.w3.org/2000/01/rdf-schema#label> ?label }"
    )
    (result,) = local_mcp(
        binary, database, [("quipu_query", {"query": query, "verbose": True})]
    )
    if result.get("truncated") is not False:
        raise ValueError("indexed pointer lookup was incomplete")
    labels = {}
    for row in result["rows"]:
        subject, label = row["s"], row["label"]
        if isinstance(subject, dict):
            subject = subject["value"]
        if isinstance(label, dict):
            label = label["value"]
        if subject in labels and labels[subject] != label:
            raise ValueError("ambiguous indexed pointer label")
        labels[subject] = label
    return labels


def materialize_release(source, revision, destination):
    """Index only exact tracked release bytes in a new private source tree."""
    destination = Path(destination)
    destination.mkdir(mode=0o700)
    archive = subprocess.run(
        ["git", "-C", str(source), "archive", "--format=tar", revision],
        check=True,
        capture_output=True,
    ).stdout
    with tarfile.open(fileobj=io.BytesIO(archive), mode="r:") as bundle:
        for member in bundle.getmembers():
            relative = Path(member.name)
            if (
                relative.is_absolute()
                or ".." in relative.parts
                or not (member.isfile() or member.isdir())
            ):
                raise ValueError("unsupported tracked release member")
            target = destination / relative
            if member.isdir():
                target.mkdir(parents=True, exist_ok=True)
            else:
                target.parent.mkdir(parents=True, exist_ok=True)
                with (
                    bundle.extractfile(member) as original,
                    target.open("xb") as copied,
                ):
                    shutil.copyfileobj(original, copied)
    return destination


def alignment_carrier(source):
    """An explicit empty reviewed carrier differs from missing or corrupt data.

    Accepted nonempty mapping transport awaits an actual curated artifact. Do
    not interpret a dependency, lexical match, or candidate as an identity.
    """
    source = Path(source)
    data = (source / "docs/knowledge/alignments.ttl").read_bytes()
    status = json.loads((source / "docs/knowledge/alignment-status.json").read_text())
    if (
        status.get("status") != "no-approved-mappings"
        or type(status.get("approved_record_count")) is not int
        or status["approved_record_count"] != 0
        or status.get("carrier_sha256") != hashlib.sha256(data).hexdigest()
    ):
        raise ValueError("unreviewed or corrupt alignment carrier")
    # Only declarations/comments are permitted in the explicit-empty profile.
    # The native data validator below still parses the complete Turtle input.
    for line in data.decode().splitlines():
        if (
            line.strip()
            and not line.lstrip().startswith("#")
            and line.strip()
            not in {
                "@prefix sssom: <https://w3id.org/sssom/> .",
                "@prefix owl: <http://www.w3.org/2002/07/owl#> .",
            }
        ):
            raise ValueError("empty alignment profile contains mapping data")
    return data.decode()


def build(revision, source, tooling, binary, bobbin, output):
    repository = "scbrown/bobbin"
    source, tooling, output = map(Path, (source, tooling, output))
    if output.exists():
        raise ValueError("repository share output already exists")
    if not re.fullmatch(r"scbrown/[a-z][a-z0-9-]*", repository):
        raise ValueError("invalid repository identity")
    slug = repository.split("/")[1]
    if (
        not re.fullmatch(r"[0-9a-f]{40}", revision)
        or command(["git", "-C", source, "rev-parse", "HEAD"]).strip() != revision
        or command(["git", "-C", source, "status", "--porcelain"]).strip()
    ):
        raise ValueError("repository source is not the exact clean release")
    paths = command(["git", "-C", source, "ls-files", "-z"]).split("\0")
    control = next((p for p in ("README.md", "README.rst") if p in paths), None)
    if control is None:
        raise ValueError("release lacks a supported public corpus positive control")
    private = output.parent / "build-private"
    private.mkdir(mode=0o700)

    def run(args, *, env=None):
        return command(args, env=env, cwd=private)

    policy = private / "policy.ttl"
    run(
        ["python3", tooling / "scripts/private-share-policy.py", policy],
        env=dict(os.environ),
    )
    corpus = materialize_release(source, revision, private / "source")
    (corpus / ".bobbin").mkdir(exist_ok=True)
    index_database = private / "indexed-graph.db"
    # Bobbin resolves embedded graph state at --source, not at the vector
    # index root. Explicit private configuration avoids global-store fallback.
    (corpus / ".bobbin/config.toml").write_text(
        "[quipu]\nstore_path = " + json.dumps(str(index_database)) + "\n"
    )
    index = private / "index"
    (index / ".bobbin").mkdir(parents=True)
    (index / ".bobbin/config.toml").write_text("""quipu_push_chunks = true

[index]
include = ["**/*.rs", "**/*.md", "**/*.rst", "**/*.toml", "**/*.yml", "**/*.yaml", "**/*.py", "**/*.sh", "**/*.js", "**/*.mjs", "**/*.ts", "**/*.tsx", "**/*.json", "**/*.ttl", "**/*.go"]
exclude = ["**/target/**", "**/.git/**", "**/.bobbin/**", "**/.agents/**", "**/.claude/**", "**/.codex/**", "**/.mcp.json", "**/CLAUDE.local.md", "**/node_modules/**", "**/book/book/**", "**/vendor/**"]
use_gitignore = true
entities = false

[embedding]
model = "all-MiniLM-L6-v2"
batch_size = 64
""")
    env = isolated_environment(private)
    run(
        [
            bobbin,
            "index",
            index,
            "--source",
            corpus,
            "--repo",
            slug,
            "--force",
            "--skip-calibrate",
            "--json",
        ],
        env=env,
    )
    database = private / "producer.db"
    if not index_database.is_file():
        raise ValueError("indexer did not produce the repository graph")
    with sqlite3.connect(
        "file:" + str(index_database) + "?mode=ro", uri=True
    ) as original, sqlite3.connect(database) as target:
        original.backup(target)
    shapes = sorted((tooling / "shapes").glob("*.ttl"))
    if not shapes:
        raise ValueError("verified engine source has no receiver shapes")
    for shape in shapes:
        run([binary, "shapes", "load", shape.stem, shape, "--db", database])
    run(
        [
            binary,
            "shapes",
            "load",
            "publication-policy",
            tooling / "docs/knowledge/publication-policy.shapes.ttl",
            "--db",
            database,
        ]
    )
    run(
        [
            binary,
            "knot",
            tooling / "docs/knowledge/publication-policy.ttl",
            "--db",
            database,
        ]
    )
    run(
        [
            binary,
            "knot",
            policy,
            "--graph",
            "urn:quipu:private-release-policy",
            "--db",
            database,
        ]
    )
    context = private / "context.ttl"
    context.write_text(f'''@prefix aegis: <http://aegis.gastown.local/ontology/> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
aegis:repo_{slug} a aegis:GitRepo ; rdfs:label "repo_{slug}" ;
  aegis:repositorySha "{revision}" .
aegis:repo-embedded-knowledge-databases-directive-2026-09-03 a aegis:Directive ;
  rdfs:label "Repository-embedded knowledge databases" ;
  aegis:issuedBy "Stiwi" ; aegis:applies_to aegis:repo_{slug} .
''')
    run([binary, "knot", context, "--db", database])
    # Curated repository content and validating shapes are repo-owned. Refuse
    # an incomplete source proposal rather than minting a pointer-only stub.
    registry_path = source / "docs/knowledge/repository-knowledge.json"
    knowledge = private / "repository-knowledge.ttl"
    constraints = source / "docs/knowledge/repository-knowledge.shapes.ttl"
    queries = source / "docs/knowledge/stored-queries.json"
    alignments = source / "docs/knowledge/alignments.ttl"
    for required in (registry_path, constraints, queries, alignments):
        if not required.is_file() or not required.stat().st_size:
            raise ValueError(
                "required repository knowledge member missing: " + required.name
            )
    import importlib.util

    spec = importlib.util.spec_from_file_location(
        "repository_knowledge", source / "scripts/build-repository-knowledge.py"
    )
    generator = importlib.util.module_from_spec(spec)
    _previous_bytecode = sys.dont_write_bytecode
    try:
        sys.dont_write_bytecode = True
        spec.loader.exec_module(generator)
    finally:
        sys.dont_write_bytecode = _previous_bytecode
    records = json.loads(registry_path.read_text())["records"]
    labels = indexed_labels(
        binary, database, sorted({record["subject"] for record in records})
    )
    knowledge.write_text(
        generator.render(source, revision, lambda subject: labels.get(subject))
        + "\n"
        + alignment_carrier(source)
    )
    run([binary, "validate", "--shapes", constraints, "--data", knowledge])
    run(
        [
            binary,
            "shapes",
            "load",
            "repository-knowledge",
            constraints,
            "--db",
            database,
        ]
    )
    run([binary, "knot", knowledge, "--db", database])
    register_queries(binary, database, json.loads(queries.read_text()))
    query = (source / "queries/repository-share-bobbin.rq").read_text()
    stage = private / "share"
    run(
        [
            binary,
            "share",
            "--construct",
            query,
            "--turtle",
            "--output",
            stage,
            "--db",
            database,
        ]
    )
    if (
        not (stage / "shapes.ttl").is_file()
        or (stage / "shapes.ttl").stat().st_size == 0
    ):
        raise ValueError("repository share omitted receiver shapes")
    receiver = private / "receiver.db"
    run(
        [
            binary,
            "shapes",
            "load",
            "repository-share",
            stage / "shapes.ttl",
            "--db",
            receiver,
        ]
    )
    imported = json.loads(run([binary, "import", stage, "--db", receiver]))
    if (
        imported.get("outcome") != "staged"
        or imported.get("promotion", {}).get("eligible") is not True
        or type(imported.get("triples", {}).get("accepted")) is not int
        or imported["triples"]["accepted"] <= 3
    ):
        raise ValueError("repository receiver did not admit a non-stub corpus")
    run([binary, "import", "promote", imported["share_id"], "--db", receiver])
    positive = run(
        [
            binary,
            "query",
            "SELECT ?s WHERE { ?s <http://aegis.gastown.local/ontology/filePath> "
            + json.dumps(control)
            + " } LIMIT 1",
            "--db",
            receiver,
        ]
    )
    if re.search(r"\n1 results\s*$", positive) is None:
        raise ValueError("repository corpus positive control missing after import")
    run(
        [
            "python3",
            tooling / "scripts/verify-repository-share-receiver.py",
            binary,
            stage,
            receiver,
            private,
        ]
    )
    if run(["git", "-C", source, "status", "--porcelain"]).strip():
        raise ValueError("indexing changed the exact repository source")
    shutil.move(stage, output)
    return {"triples": imported["triples"]["accepted"], "positive_file": control}


if __name__ == "__main__":
    import sys

    if len(sys.argv) != 7:
        raise SystemExit(
            "usage: build-repository-share.py <revision> <source> <engine-source> <quipu-bin> <bobbin-bin> <new-output>"
        )
    build(*sys.argv[1:])
