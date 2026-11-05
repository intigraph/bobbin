"""Explicit private provider bytes for an opted-in repository builder.

This validates and snapshots bytes; it never loads a model or enables a publisher.
"""

import hashlib
import json
import os
from pathlib import Path
import re
import stat


def snapshot(path, expected, destination, maximum):
    """Hash the same bounded, regular-file bytes that enter the private copy."""
    source = Path(path)
    if not source.is_absolute() or not re.fullmatch(r"[0-9a-f]{64}", expected):
        raise ValueError("provider requires absolute paths and explicit SHA256 pins")
    descriptor = os.open(source, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    with os.fdopen(descriptor, "rb") as reader:
        before = os.fstat(reader.fileno())
        if not stat.S_ISREG(before.st_mode) or not 0 < before.st_size <= maximum:
            raise ValueError("provider input must be a bounded nonempty regular file")
        digest = hashlib.sha256()
        total = 0
        with destination.open("xb") as writer:
            destination.chmod(0o400)
            while chunk := reader.read(1024 * 1024):
                total += len(chunk)
                if total > maximum:
                    raise ValueError("provider input exceeds its size limit")
                digest.update(chunk)
                writer.write(chunk)
        after = os.fstat(reader.fileno())
        if (before.st_size, before.st_mtime_ns, before.st_ctime_ns) != (
            after.st_size, after.st_mtime_ns, after.st_ctime_ns
        ) or total != before.st_size or digest.hexdigest() != expected:
            raise ValueError("provider input changed or does not match its SHA256 pin")


def configure(private, environment=None):
    """Create an explicit project-local Quipu recipe; never inherit host config."""
    env = os.environ if environment is None else environment
    required = (
        "QUIPU_SHARE_MODEL_PATH", "QUIPU_SHARE_MODEL_SHA256",
        "QUIPU_SHARE_TOKENIZER_PATH", "QUIPU_SHARE_TOKENIZER_SHA256",
        "QUIPU_SHARE_EMBEDDING_DIMENSION",
    )
    if any(not isinstance(env.get(key), str) or not env[key] for key in required):
        raise ValueError("explicit repository embedding provider contract is required")
    dimension = env[required[4]]
    if not re.fullmatch(r"[1-9][0-9]{0,3}", dimension) or int(dimension) > 4096:
        raise ValueError("provider dimension must be an integer from 1 to 4096")
    private = Path(private)
    provider = private / "provider"
    provider.mkdir(mode=0o700)
    model, tokenizer = provider / "model.onnx", provider / "tokenizer.json"
    snapshot(env[required[0]], env[required[1]], model, 1024 * 1024 * 1024)
    snapshot(env[required[2]], env[required[3]], tokenizer, 32 * 1024 * 1024)
    config_dir = private / ".bobbin"
    config_dir.mkdir(mode=0o700)
    config = config_dir / "config.toml"
    with config.open("x") as handle:
        config.chmod(0o600)
        handle.write("[quipu.embedding]\n")
        handle.write("model_path = " + json.dumps(str(model)) + "\n")
        handle.write("tokenizer_path = " + json.dumps(str(tokenizer)) + "\n")
        handle.write("dimension = " + dimension + "\nauto_embed = true\n")
    return {
        "model": model.name,
        "model_sha256": env[required[1]],
        "tokenizer_sha256": env[required[3]],
        "dimension": int(dimension),
    }


def verify_manifest(stage, expected):
    """A successful engine exit alone cannot establish the pinned provider."""
    actual = json.loads((Path(stage) / "manifest.json").read_text()).get("embedding")
    if actual != expected:
        raise ValueError("share embedding recipe does not match the explicit provider pins")


def configure_indexer(index, private, expected):
    """Use the same verified private bytes for indexing, without cache downloads."""
    config = Path(index) / ".bobbin/config.toml"
    with config.open("a") as handle:
        handle.write("dimensions = " + str(expected["dimension"]) + "\n")
        handle.write("[embedding.custom_model]\n")
        handle.write("model_path = " + json.dumps(str(Path(private) / "provider/model.onnx")) + "\n")
        handle.write("tokenizer_path = " + json.dumps(str(Path(private) / "provider/tokenizer.json")) + "\n")
        handle.write("max_seq_len = 256\n")
