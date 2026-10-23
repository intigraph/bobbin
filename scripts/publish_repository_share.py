#!/usr/bin/env python3
"""Bobbin's owned trusted release job; hosted workflows only verify its output."""

import gzip
import hashlib
import importlib.util
import json
import os
import re
import shutil
import subprocess
import sys
import tarfile
import tempfile
from pathlib import Path

REPOSITORY = "scbrown/bobbin"
MEMBERS = {
    "export.nt",
    "export.ttl",
    "payload.nq",
    "manifest.json",
    "manifest.ttl",
    "shapes.ttl",
    "queries.ttl",
    "alignments.ttl",
}


def command(args):
    return subprocess.run(
        [str(v) for v in args], check=True, capture_output=True, text=True
    ).stdout


def digest(path):
    result = hashlib.sha256()
    with path.open("rb") as handle:
        for block in iter(lambda: handle.read(1 << 20), b""):
            result.update(block)
    return result.hexdigest()


def check_share(stage):
    files = list(stage.iterdir())
    if any(not p.is_file() or p.is_symlink() or p.name not in MEMBERS for p in files):
        raise ValueError("unexpected or non-regular share member")
    for name in (
        "export.nt",
        "manifest.json",
        "manifest.ttl",
        "shapes.ttl",
        "queries.ttl",
    ):
        if not (stage / name).is_file() or not (stage / name).stat().st_size:
            raise ValueError("required nonempty share member missing: " + name)
    manifest = json.loads((stage / "manifest.json").read_text())
    # A native engine with --alignments carries the reviewed file; it is only
    # admitted when the manifest declares it and binds its exact bytes.
    carried = stage / "alignments.ttl"
    declared = manifest.get("files", {}).get("alignments")
    if carried.exists() or declared is not None or "alignments_hash" in manifest:
        if (
            declared != "alignments.ttl"
            or not carried.is_file()
            or manifest.get("alignments_hash") != "sha256:" + digest(carried)
        ):
            raise ValueError("alignments member is undeclared, missing or not hash-bound")
    recipe = manifest.get("embedding")
    if (
        not isinstance(recipe, dict)
        or not isinstance(recipe.get("model"), str)
        or not recipe["model"]
        or Path(recipe["model"]).name != recipe["model"]
        or type(recipe.get("dimension")) is not int
        or recipe["dimension"] <= 0
        or any(
            not re.fullmatch("[0-9a-f]{64}", recipe.get(k, ""))
            for k in ("model_sha256", "tokenizer_sha256")
        )
    ):
        raise ValueError("share omitted a provider-byte embedding recipe")
    # This repository pack is pointer-only. Curated idea summaries use
    # rdfs:comment; source bodies and line spans are never release predicates.
    export = (stage / "export.nt").read_text()
    for predicate in ("content", "startLine", "endLine"):
        if "<http://aegis.gastown.local/ontology/" + predicate + ">" in export:
            raise ValueError("share duplicates source text or line-span data")
    return recipe


def package(stage, archive):
    check_share(stage)
    with archive.open("xb") as raw:
        with gzip.GzipFile(filename="", fileobj=raw, mode="wb", mtime=0) as compressed:
            with tarfile.open(fileobj=compressed, mode="w") as tar:
                for path in sorted(stage.iterdir()):
                    info = tar.gettarinfo(str(path), arcname=path.name)
                    info.uid = info.gid = 0
                    info.uname = info.gname = ""
                    info.mtime = 0
                    info.mode = 0o600
                    with path.open("rb") as handle:
                        tar.addfile(info, handle)


def held():
    path = Path(
        os.environ.get("QUIPU_SHARE_HOLD_FILE", str(Path.home() / ".quipu-share-hold"))
    )
    try:
        path.lstat()
    except FileNotFoundError:
        return False
    except OSError as error:
        raise ValueError("cannot inspect publication hold") from error
    return True


def publish(tag, revision, output):
    if held():
        raise ValueError("publication held before any lookup/build")
    if not re.fullmatch(r"v\d+\.\d+\.\d+(?:[.-][A-Za-z0-9.-]+)?", tag):
        raise ValueError("unexpected Bobbin release tag")
    if not re.fullmatch("[0-9a-f]{40}", revision):
        raise ValueError("invalid release revision")
    source = Path(__file__).resolve().parents[1]
    if (
        command(["git", "-C", source, "rev-parse", "HEAD"]).strip() != revision
        or command(
            ["git", "-C", source, "rev-parse", "refs/tags/" + tag + "^{commit}"]
        ).strip()
        != revision
        or command(["git", "-C", source, "status", "--porcelain"]).strip()
    ):
        raise ValueError("publisher source is not the exact clean release")
    output = Path(output)
    if output.exists():
        raise ValueError("retained output exists; reconcile before retrying")
    binary = Path(os.environ["QUIPU_SHARE_ENGINE_BIN"]).resolve(strict=True)
    tooling = Path(os.environ["QUIPU_SHARE_ENGINE_SOURCE"]).resolve(strict=True)
    bobbin = Path(os.environ["QUIPU_SHARE_BOBBIN"]).resolve(strict=True)
    engine_revision = os.environ["QUIPU_SHARE_ENGINE_REVISION"]
    if (
        not re.fullmatch("[0-9a-f]{40}", engine_revision)
        or command(["git", "-C", tooling, "rev-parse", "HEAD"]).strip()
        != engine_revision
        or command(["git", "-C", tooling, "status", "--porcelain"]).strip()
        or digest(binary) != os.environ["QUIPU_SHARE_ENGINE_BIN_SHA256"]
        or digest(binary.with_name("quipu-server"))
        != os.environ["QUIPU_SHARE_ENGINE_SERVER_SHA256"]
        or digest(bobbin) != os.environ["QUIPU_SHARE_BOBBIN_SHA256"]
    ):
        raise ValueError("trusted engine/source/indexer pin mismatch")
    output.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
    with tempfile.TemporaryDirectory(prefix="prepare-", dir=output.parent) as temp:
        private = Path(temp)
        stage = private / "share"
        spec = importlib.util.spec_from_file_location(
            "bobbin_share_build", source / "scripts/build-repository-share.py"
        )
        builder = importlib.util.module_from_spec(spec)
        _previous_bytecode = sys.dont_write_bytecode
        try:
            sys.dont_write_bytecode = True
            spec.loader.exec_module(builder)
        finally:
            sys.dont_write_bytecode = _previous_bytecode
        builder.build(revision, source, tooling, binary, bobbin, stage)
        archive_name = "bobbin-" + tag + "-repository.pendant.tar.gz"
        prepared = private / "assets"
        prepared.mkdir(mode=0o700)
        archive = prepared / archive_name
        package(stage, archive)
        (prepared / (archive_name + ".sha256")).write_text(
            digest(archive) + "  " + archive_name + "\n"
        )
        proof = {
            "schema": "quipu.repository-share-producer/v1",
            "source_repository": REPOSITORY,
            "source_revision": revision,
            "release_tag": tag,
            "archive_sha256": digest(archive),
            "quipu_sha256": digest(binary),
            "engine_source_revision": engine_revision,
            "bobbin_sha256": digest(bobbin),
            "quipu_server_sha256": digest(binary.with_name("quipu-server")),
            "destination": "outward",
        }
        (prepared / (archive_name + ".provenance.json")).write_text(
            json.dumps(proof, sort_keys=True, indent=2) + "\n"
        )
        names = sorted(p.name for p in prepared.iterdir())
        if held():
            raise ValueError("publication held after preparation")
        available = json.loads(
            command(
                ["gh", "release", "view", tag, "--repo", REPOSITORY, "--json", "assets"]
            )
        )
        present = {a["name"] for a in available["assets"]} & set(names)
        if present and present != set(names):
            raise ValueError("partial publication; explicit reconciliation required")
        # Retain before the one-way upload. No --clobber and no automatic retry
        # from an uncertain write. The managed dispatcher also retains STARTED.
        shutil.copytree(prepared, output)
        if not present:
            if held():
                raise ValueError("publication held immediately before upload")
            command(
                [
                    "gh",
                    "release",
                    "upload",
                    tag,
                    "--repo",
                    REPOSITORY,
                    *(output / name for name in names),
                ]
            )
        downloaded = private / "downloaded"
        downloaded.mkdir()
        command(
            [
                "gh",
                "release",
                "download",
                tag,
                "--repo",
                REPOSITORY,
                "--dir",
                downloaded,
                *(arg for name in names for arg in ("--pattern", name)),
            ]
        )
        for name in names:
            if (downloaded / name).read_bytes() != (output / name).read_bytes():
                raise ValueError(
                    "existing or newly published asset differs; never overwrite"
                )
        receipt = {
            **proof,
            "published_and_read_back": True,
            "assets": names,
            "asset_sha256": {name: digest(downloaded / name) for name in names},
        }
        (output / "delivery.json").write_text(json.dumps(receipt, indent=2) + "\n")
        return receipt


if __name__ == "__main__":
    if len(sys.argv) != 5 or sys.argv[1] != "--trusted-release":
        raise SystemExit(
            "usage: publish-repository-share.sh --trusted-release <tag> <revision> <new-private-state>"
        )
    print(json.dumps(publish(sys.argv[2], sys.argv[3], sys.argv[4])))
