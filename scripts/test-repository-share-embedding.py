#!/usr/bin/env python3
"""Private byte/config controls; synthetic bytes are not native ONNX acceptance.

# arming: ci; invoked by the existing repository publication test entrypoint.
"""

import hashlib
import importlib.util
import json
import os
from pathlib import Path
import tempfile
import tomllib
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parent


def load(name, filename):
    spec = importlib.util.spec_from_file_location(name, ROOT / filename)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


provider = load("provider_test", "repository_share_embedding.py")
builder = load("provider_builder_test", "build-repository-share.py")


class ProviderContract(unittest.TestCase):
    def setUp(self):
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        self.root = Path(temp.name)
        self.private = self.root / "private"
        self.private.mkdir(mode=0o700)
        self.model = self.root / "public-model"
        self.tokenizer = self.root / "public-tokenizer"
        self.model.write_bytes(b"inert fixture, never passed to ONNX")
        self.tokenizer.write_bytes(b'{"inert":"fixture"}')
        self.env = {
            "QUIPU_SHARE_MODEL_PATH": str(self.model),
            "QUIPU_SHARE_MODEL_SHA256": hashlib.sha256(self.model.read_bytes()).hexdigest(),
            "QUIPU_SHARE_TOKENIZER_PATH": str(self.tokenizer),
            "QUIPU_SHARE_TOKENIZER_SHA256": hashlib.sha256(self.tokenizer.read_bytes()).hexdigest(),
            "QUIPU_SHARE_EMBEDDING_DIMENSION": "384",
        }

    def test_indexer_uses_the_same_private_bytes_without_builtin_cache(self):
        expected = provider.configure(self.private, self.env)
        index = self.root / "index"
        (index / ".bobbin").mkdir(parents=True)
        config = index / ".bobbin/config.toml"
        config.write_text('[embedding]\nmodel = "all-MiniLM-L6-v2"\n')
        provider.configure_indexer(index, self.private, expected)
        embedding = tomllib.loads(config.read_text())["embedding"]
        self.assertEqual(embedding["dimensions"], 384)
        self.assertEqual(embedding["custom_model"]["model_path"], str(self.private / "provider/model.onnx"))
        self.assertEqual(embedding["custom_model"]["tokenizer_path"], str(self.private / "provider/tokenizer.json"))
        self.assertEqual(embedding["custom_model"]["max_seq_len"], 256)

    def test_private_snapshot_config_and_exact_manifest(self):
        expected = provider.configure(self.private, self.env)
        config = self.private / ".bobbin/config.toml"
        embedding = tomllib.loads(config.read_text())["quipu"]["embedding"]
        self.assertTrue(embedding["auto_embed"])
        self.assertEqual(embedding["dimension"], 384)
        for field, source in [("model_path", self.model), ("tokenizer_path", self.tokenizer)]:
            copied = Path(embedding[field])
            self.assertTrue(copied.is_relative_to(self.private))
            self.assertEqual(copied.read_bytes(), source.read_bytes())
            self.assertNotEqual(copied.stat().st_ino, source.stat().st_ino)
            self.assertEqual(copied.stat().st_mode & 0o777, 0o400)
        self.assertEqual(config.stat().st_mode & 0o777, 0o600)
        self.assertEqual(config.parent.stat().st_mode & 0o777, 0o700)
        self.model.write_bytes(b"host input changed after snapshot")
        self.assertEqual(hashlib.sha256(Path(embedding["model_path"]).read_bytes()).hexdigest(),
                         expected["model_sha256"])
        stage = self.root / "share"
        stage.mkdir()
        manifest = stage / "manifest.json"
        manifest.write_text(json.dumps({"embedding": expected}))
        provider.verify_manifest(stage, expected)
        for wrong in [None, {}, {**expected, "dimension": 768},
                      {**expected, "model_sha256": "f" * 64},
                      {**expected, "tokenizer_sha256": "f" * 64},
                      {**expected, "model": "other.onnx"}]:
            manifest.write_text(json.dumps({"embedding": wrong}))
            with self.subTest(wrong=wrong), self.assertRaisesRegex(ValueError, "recipe"):
                provider.verify_manifest(stage, expected)

    def test_missing_fields_refuse_without_ambient_fallback(self):
        for key in self.env:
            env = {k: v for k, v in self.env.items() if k != key}
            with self.subTest(key=key), self.assertRaisesRegex(ValueError, "required"):
                provider.configure(self.private, env)
        self.assertEqual(list(self.private.iterdir()), [])

    def test_dimension_refusal_before_writing(self):
        for bad in ["0", "-1", "4097", "384.0", "true", " 384", "384\n"]:
            with self.subTest(bad=bad), self.assertRaisesRegex(ValueError, "dimension"):
                provider.configure(self.private, {**self.env, "QUIPU_SHARE_EMBEDDING_DIMENSION": bad})
        self.assertEqual(list(self.private.iterdir()), [])

    def test_mismatched_digest_never_creates_config(self):
        self.env["QUIPU_SHARE_MODEL_SHA256"] = "f" * 64
        with self.assertRaisesRegex(ValueError, "SHA256"):
            provider.configure(self.private, self.env)
        self.assertFalse((self.private / ".bobbin/config.toml").exists())

    def test_relative_path_and_unpinned_digest_refuse(self):
        for path, digest in [("relative", "a" * 64), (str(self.model), "A" * 64),
                             (str(self.model), ""), (str(self.model), "sha256:" + "a" * 64)]:
            with self.subTest(path=path, digest=digest), self.assertRaisesRegex(ValueError, "pins"):
                provider.snapshot(path, digest, self.private / "out", 1024)
        self.assertEqual(list(self.private.iterdir()), [])

    def test_symlink_empty_directory_and_oversize_inputs_refuse(self):
        link = self.root / "link"
        link.symlink_to(self.model)
        empty = self.root / "empty"
        empty.touch()
        for path, maximum in [(link, 1024), (empty, 1024), (self.root, 1024), (self.model, 1)]:
            with self.subTest(path=path), self.assertRaises((OSError, ValueError)):
                provider.snapshot(str(path), self.env["QUIPU_SHARE_MODEL_SHA256"],
                                  self.private / "out", maximum)
        self.assertEqual(list(self.private.iterdir()), [])

    def test_existing_configuration_is_not_overwritten(self):
        directory = self.private / ".bobbin"
        directory.mkdir()
        config = directory / "config.toml"
        config.write_text("existing private configuration\n")
        with self.assertRaises(FileExistsError):
            provider.configure(self.private, self.env)
        self.assertEqual(config.read_text(), "existing private configuration\n")

    def test_mutation_during_copy_is_refused(self):
        original_sha256 = hashlib.sha256
        model = self.model

        class MutatingDigest:
            def __init__(self):
                self.digest = original_sha256()

            def update(self, chunk):
                self.digest.update(chunk)
                model.write_bytes(b"changed while copying")

            def hexdigest(self):
                return self.digest.hexdigest()

        with patch.object(provider.hashlib, "sha256", MutatingDigest):
            with self.assertRaisesRegex(ValueError, "changed"):
                provider.configure(self.private, self.env)
        self.assertFalse((self.private / ".bobbin/config.toml").exists())

    def test_builder_refuses_missing_contract_before_policy_or_indexer(self):
        source = self.root / "source"
        source.mkdir()
        revision = "a" * 40
        (self.root / "output").mkdir()

        def metadata_only(args, **kwargs):
            if list(args[:1]) != ["git"]:
                self.fail("external policy/indexer/engine invoked without a provider")
            if args[-2:] == ["rev-parse", "HEAD"]:
                return revision
            if args[-2:] == ["status", "--porcelain"]:
                return ""
            if args[-2:] == ["ls-files", "-z"]:
                return "README.md\0"
            self.fail("unexpected metadata command")

        with patch.dict(os.environ, {}, clear=True), patch.object(builder, "command", metadata_only):
            with self.assertRaisesRegex(ValueError, "contract is required"):
                builder.build(revision, source, source, "unused-engine", "unused-indexer",
                              self.root / "output" / "share")


if __name__ == "__main__":
    unittest.main()
