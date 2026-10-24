"""Tests for eval.runner.bobbin_setup module.

Uses mocks for subprocess since we don't invoke real bobbin binary.
"""

from __future__ import annotations

import json
import subprocess
from pathlib import Path
from unittest.mock import MagicMock, patch

import pytest

from runner.bobbin_setup import (
    ALLOWED_OVERRIDE_KEYS,
    BobbinSetupError,
    _find_bobbin,
    _format_toml_value,
    _parse_profile,
    _set_toml_value,
    apply_config_overrides,
    generate_override_settings,
    parse_config_override,
    setup_bobbin,
)


def test_l0_no_coupling_override_changes_index_config(tmp_path):
    import tomllib

    config = tmp_path / ".bobbin" / "config.toml"
    config.parent.mkdir()
    config.write_text("[git]\ncoupling_enabled = true\ncoupling_depth = 7\n")
    apply_config_overrides(str(tmp_path), {"git.coupling_enabled": "false"})
    parsed = tomllib.loads(config.read_text())
    assert parsed["git"]["coupling_enabled"] is False
    assert parsed["git"]["coupling_depth"] == 7


class TestFindBobbin:
    def test_found_on_path(self):
        with patch("runner.bobbin_setup.shutil.which", return_value="/usr/bin/bobbin"):
            assert _find_bobbin() == "/usr/bin/bobbin"

    def test_found_in_cargo_bin(self, tmp_path: Path):
        fake_bin = tmp_path / ".cargo" / "bin" / "bobbin"
        fake_bin.parent.mkdir(parents=True)
        fake_bin.touch()

        with (
            patch("runner.bobbin_setup.shutil.which", return_value=None),
            patch("runner.bobbin_setup.Path.home", return_value=tmp_path),
        ):
            assert _find_bobbin() == str(fake_bin)

    def test_not_found_raises(self, tmp_path: Path):
        with (
            patch("runner.bobbin_setup.shutil.which", return_value=None),
            patch("runner.bobbin_setup.Path.home", return_value=tmp_path),
        ):
            with pytest.raises(BobbinSetupError, match="bobbin binary not found"):
                _find_bobbin()


class TestSetupBobbin:
    @pytest.fixture()
    def mock_bobbin(self):
        """Patch _find_bobbin, subprocess.run, and subprocess.Popen.

        setup_bobbin uses subprocess.run for init and status, and
        subprocess.Popen for the bounded index communication.
        """
        popen_inst = MagicMock()
        popen_inst.stderr = iter([])
        popen_inst.stdout.read.return_value = ""
        popen_inst.communicate.return_value = ("", "")
        popen_inst.returncode = 0

        with (
            patch("runner.bobbin_setup._find_bobbin", return_value="/usr/bin/bobbin"),
            patch("runner.bobbin_setup.subprocess.run") as mock_run,
            patch("runner.bobbin_setup.subprocess.Popen") as mock_popen,
            patch("runner.bobbin_setup.os.killpg"),
        ):
            mock_run.return_value = subprocess.CompletedProcess(
                args=[], returncode=0, stdout="", stderr="",
            )
            mock_popen.return_value = popen_inst
            yield mock_run, mock_popen

    def test_runs_init_then_index(self, mock_bobbin, tmp_path: Path):
        mock_run, mock_popen = mock_bobbin
        result = setup_bobbin(str(tmp_path))

        # init + status via subprocess.run
        assert mock_run.call_count == 2
        init_cmd = mock_run.call_args_list[0][0][0]
        status_cmd = mock_run.call_args_list[1][0][0]
        assert init_cmd == ["/usr/bin/bobbin", "init"]
        assert status_cmd == ["/usr/bin/bobbin", "status", "--json"]

        # index via subprocess.Popen
        assert mock_popen.call_count == 1
        index_cmd = mock_popen.call_args[0][0]
        assert index_cmd == ["/usr/bin/bobbin", "index", "--verbose"]

        assert isinstance(result, dict)
        assert "index_duration_seconds" in result

    def test_workspace_used_as_cwd(self, mock_bobbin, tmp_path: Path):
        mock_run, mock_popen = mock_bobbin
        setup_bobbin(str(tmp_path))
        for c in mock_run.call_args_list:
            assert c[1]["cwd"] == tmp_path
        assert mock_popen.call_args[1]["cwd"] == tmp_path

    def test_resume_indexes_without_reinitializing_config(self, mock_bobbin, tmp_path):
        mock_run, mock_popen = mock_bobbin
        config = tmp_path / ".bobbin/config.toml"
        config.parent.mkdir()
        config.write_text("[git]\ncoupling_enabled = false\n")
        setup_bobbin(str(tmp_path), initialize=False)
        assert mock_run.call_count == 1
        assert mock_run.call_args.args[0] == ["/usr/bin/bobbin", "status", "--json"]
        mock_popen.assert_called_once()
        assert config.read_text() == "[git]\ncoupling_enabled = false\n"

    def test_init_failure_raises(self, mock_bobbin, tmp_path: Path):
        mock_run, mock_popen = mock_bobbin
        mock_run.side_effect = subprocess.CalledProcessError(
            returncode=1, cmd=["bobbin", "init"], stderr="bad config",
        )

        with pytest.raises(BobbinSetupError, match="bobbin init failed"):
            setup_bobbin(str(tmp_path))

    def test_init_timeout_is_a_setup_error_with_the_callers_budget(self, mock_bobbin, tmp_path):
        # aegis-bgk9ho: a GPU wrapper's runtime verification runs inside init.
        mock_run, _ = mock_bobbin
        mock_run.side_effect = subprocess.TimeoutExpired(
            cmd=["bobbin", "init"], timeout=900, stderr=b"GPU paused")
        with pytest.raises(BobbinSetupError, match="init timed out after 900s"):
            setup_bobbin(str(tmp_path), init_timeout=900)
        assert mock_run.call_args.kwargs["timeout"] == 900

    def test_init_timeout_default_is_unchanged(self, mock_bobbin, tmp_path):
        mock_run, _ = mock_bobbin
        setup_bobbin(str(tmp_path))
        assert mock_run.call_args_list[0].kwargs["timeout"] == 30

    def test_index_failure_raises(self, mock_bobbin, tmp_path: Path):
        mock_run, mock_popen = mock_bobbin
        # Make the Popen index step return non-zero.
        popen_inst = mock_popen.return_value
        popen_inst.returncode = 1

        with pytest.raises(BobbinSetupError, match="bobbin index failed"):
            setup_bobbin(str(tmp_path))

    def test_index_timeout_raises(self, mock_bobbin, tmp_path: Path):
        mock_run, mock_popen = mock_bobbin
        popen_inst = mock_popen.return_value
        popen_inst.communicate.side_effect = [subprocess.TimeoutExpired(
            cmd=["bobbin", "index"], timeout=300,
        ), ("", "")]

        with pytest.raises(BobbinSetupError, match="timed out"):
            setup_bobbin(str(tmp_path))

    def test_returns_metadata_with_status(self, mock_bobbin, tmp_path: Path):
        """setup_bobbin returns metadata dict including bobbin status info."""
        import json
        mock_run, mock_popen = mock_bobbin
        status_json = json.dumps({
            "total_files": 42,
            "total_chunks": 100,
            "total_embeddings": 100,
            "languages": ["Rust", "Python"],
        })
        mock_run.side_effect = [
            subprocess.CompletedProcess(args=[], returncode=0, stdout="", stderr=""),
            subprocess.CompletedProcess(args=[], returncode=0, stdout=status_json, stderr=""),
        ]
        result = setup_bobbin(str(tmp_path))
        assert result["total_files"] == 42
        assert result["total_chunks"] == 100
        assert result["languages"] == ["Rust", "Python"]

    def test_custom_timeout(self, mock_bobbin, tmp_path: Path):
        mock_run, mock_popen = mock_bobbin
        setup_bobbin(str(tmp_path), timeout=120)

        # communicate must bound execution while both pipes drain.
        popen_inst = mock_popen.return_value
        popen_inst.communicate.assert_called_once_with(timeout=120)

    def test_returns_profile_when_available(self, mock_bobbin, tmp_path: Path):
        """setup_bobbin captures profiling data from -v output."""
        mock_run, mock_popen = mock_bobbin
        verbose_output = (
            "  Checking embedding model...\n"
            "  Found 50 files matching patterns\n"
            "\n"
            "Profile:\n"
            "  file I/O:         10ms\n"
            "  parse:            20ms\n"
            "  context:           5ms\n"
            "  embed:           100ms  (200 chunks in 4 batches)\n"
            "    tokenize:       30ms\n"
            "    inference:      65ms\n"
            "    pooling:         5ms\n"
            "  lance delete:     15ms\n"
            "  lance insert:     30ms\n"
            "  git coupling:     50ms\n"
            "  git commits:      25ms\n"
            "  deps:             10ms\n"
            "  compact:          40ms\n"
            "  other/overhead:    5ms\n"
            "  TOTAL:           310ms\n"
            "  embed throughput: 2000.0 chunks/s\n"
        )
        popen_inst = mock_popen.return_value
        popen_inst.communicate.return_value = (verbose_output, "")

        mock_run.side_effect = [
            subprocess.CompletedProcess(args=[], returncode=0, stdout="", stderr=""),
            subprocess.CompletedProcess(args=[], returncode=0, stdout="{}", stderr=""),
        ]
        result = setup_bobbin(str(tmp_path))
        assert "profile" in result
        assert result["profile"]["file_i/o"] == 10
        assert result["profile"]["embed"] == 100
        assert result["profile"]["inference"] == 65
        assert result["profile"]["total_ms"] == 310
        assert result["profile"]["embed_throughput_chunks_per_sec"] == 2000.0


class TestParseProfile:
    def test_parses_full_output(self):
        output = (
            "Profile:\n"
            "  file I/O:         10ms\n"
            "  parse:            20ms\n"
            "  context:           5ms\n"
            "  embed:           100ms  (200 chunks in 4 batches)\n"
            "    tokenize:       30ms\n"
            "    inference:      65ms\n"
            "    pooling:         5ms\n"
            "  lance delete:     15ms\n"
            "  lance insert:     30ms\n"
            "  git coupling:     50ms\n"
            "  git commits:      25ms\n"
            "  deps:             10ms\n"
            "  compact:          40ms\n"
            "  other/overhead:    5ms\n"
            "  TOTAL:           310ms\n"
            "  embed throughput: 2000.0 chunks/s\n"
        )
        result = _parse_profile(output)
        assert result is not None
        assert result["file_i/o"] == 10
        assert result["parse"] == 20
        assert result["context"] == 5
        assert result["embed"] == 100
        assert result["tokenize"] == 30
        assert result["inference"] == 65
        assert result["pooling"] == 5
        assert result["lance_delete"] == 15
        assert result["lance_insert"] == 30
        assert result["git_coupling"] == 50
        assert result["git_commits"] == 25
        assert result["deps"] == 10
        assert result["compact"] == 40
        assert result["other/overhead"] == 5
        assert result["total_ms"] == 310
        assert result["embed_throughput_chunks_per_sec"] == 2000.0

    def test_returns_none_for_no_profile(self):
        assert _parse_profile("just some output\nno profile here") is None

    def test_returns_none_for_empty_string(self):
        assert _parse_profile("") is None


_SAMPLE_CONFIG = """\
[index]
use_gitignore = true

[search]
semantic_weight = 0.7
recency_weight = 0.3
doc_demotion = 0.5
rrf_k = 60.0

[git]
coupling_enabled = true
coupling_depth = 5000

[hooks]
gate_threshold = 0.75
show_docs = true
"""


class TestParseConfigOverride:
    def test_valid_override(self):
        key, val = parse_config_override("semantic_weight=0.5")
        assert key == "semantic_weight"
        assert val == "0.5"

    def test_strips_whitespace(self):
        key, val = parse_config_override("  gate_threshold = 1.0 ")
        assert key == "gate_threshold"
        assert val == "1.0"

    def test_unknown_key_raises(self):
        with pytest.raises(ValueError, match="Unknown override key"):
            parse_config_override("bogus_key=42")

    def test_no_equals_raises(self):
        with pytest.raises(ValueError, match="Invalid override format"):
            parse_config_override("semantic_weight")

    def test_all_keys_valid(self):
        for key in ALLOWED_OVERRIDE_KEYS:
            k, _ = parse_config_override(f"{key}=1")
            assert k == key


class TestSetTomlValue:
    def test_replace_existing_key(self):
        result = _set_toml_value(_SAMPLE_CONFIG, "search", "semantic_weight", 0.0)
        assert "semantic_weight = 0.0" in result
        assert "semantic_weight = 0.7" not in result

    def test_add_missing_key_to_existing_section(self):
        result = _set_toml_value(_SAMPLE_CONFIG, "search", "new_param", 42)
        assert "new_param = 42" in result
        # Should be between [search] and [git]
        lines = result.splitlines()
        search_idx = next(i for i, l in enumerate(lines) if l.strip() == "[search]")
        git_idx = next(i for i, l in enumerate(lines) if l.strip() == "[git]")
        new_idx = next(i for i, l in enumerate(lines) if "new_param" in l)
        assert search_idx < new_idx <= git_idx

    def test_add_missing_section_and_key(self):
        result = _set_toml_value(_SAMPLE_CONFIG, "new_section", "foo", True)
        assert "[new_section]" in result
        assert "foo = true" in result

    def test_replace_int_value(self):
        result = _set_toml_value(_SAMPLE_CONFIG, "git", "coupling_depth", 0)
        assert "coupling_depth = 0" in result
        assert "coupling_depth = 5000" not in result

    def test_replace_bool_value(self):
        result = _set_toml_value(_SAMPLE_CONFIG, "hooks", "show_docs", False)
        assert "show_docs = false" in result
        assert "show_docs = true" not in result


class TestFormatTomlValue:
    def test_bool_true(self):
        assert _format_toml_value(True) == "true"

    def test_bool_false(self):
        assert _format_toml_value(False) == "false"

    def test_int(self):
        assert _format_toml_value(42) == "42"

    def test_float(self):
        result = _format_toml_value(0.5)
        assert "." in result
        assert float(result) == 0.5

    def test_float_zero(self):
        result = _format_toml_value(0.0)
        assert "." in result
        assert float(result) == 0.0


class TestApplyConfigOverrides:
    def test_modifies_config_file(self, tmp_path: Path):
        config_dir = tmp_path / ".bobbin"
        config_dir.mkdir()
        (config_dir / "config.toml").write_text(_SAMPLE_CONFIG)

        apply_config_overrides(str(tmp_path), {"semantic_weight": "0.0"})

        content = (config_dir / "config.toml").read_text()
        assert "semantic_weight = 0.0" in content

    def test_multiple_overrides(self, tmp_path: Path):
        config_dir = tmp_path / ".bobbin"
        config_dir.mkdir()
        (config_dir / "config.toml").write_text(_SAMPLE_CONFIG)

        apply_config_overrides(str(tmp_path), {
            "semantic_weight": "0.0",
            "gate_threshold": "1.0",
            "coupling_depth": "0",
        })

        content = (config_dir / "config.toml").read_text()
        assert "semantic_weight = 0.0" in content
        assert "gate_threshold = 1.0" in content
        assert "coupling_depth = 0" in content

    def test_missing_config_raises(self, tmp_path: Path):
        with pytest.raises(BobbinSetupError, match="Config not found"):
            apply_config_overrides(str(tmp_path), {"semantic_weight": "0.0"})

    def test_blame_bridging_maps_to_show_docs(self, tmp_path: Path):
        config_dir = tmp_path / ".bobbin"
        config_dir.mkdir()
        (config_dir / "config.toml").write_text(_SAMPLE_CONFIG)

        apply_config_overrides(str(tmp_path), {"blame_bridging": "false"})

        content = (config_dir / "config.toml").read_text()
        assert "show_docs = false" in content


class TestGenerateOverrideSettings:
    def test_rewrites_gate_threshold(self, tmp_path: Path):
        base = tmp_path / "settings.json"
        base.write_text(json.dumps({
            "hooks": {
                "UserPromptSubmit": [{
                    "hooks": [{
                        "command": "bobbin hook inject-context --gate-threshold 0.0 --show-docs false",
                        "type": "command",
                    }],
                }],
            },
        }))

        out = tmp_path / "override.json"
        generate_override_settings(str(base), {"gate_threshold": "0.9"}, str(out))

        settings = json.loads(out.read_text())
        cmd = settings["hooks"]["UserPromptSubmit"][0]["hooks"][0]["command"]
        assert "--gate-threshold 0.9" in cmd
        assert "--gate-threshold 0.0" not in cmd

    def test_rewrites_blame_bridging_to_show_docs(self, tmp_path: Path):
        base = tmp_path / "settings.json"
        base.write_text(json.dumps({
            "hooks": {
                "UserPromptSubmit": [{
                    "hooks": [{
                        "command": "bobbin hook inject-context --gate-threshold 0.0 --show-docs false",
                        "type": "command",
                    }],
                }],
            },
        }))

        out = tmp_path / "override.json"
        generate_override_settings(str(base), {"blame_bridging": "true"}, str(out))

        settings = json.loads(out.read_text())
        cmd = settings["hooks"]["UserPromptSubmit"][0]["hooks"][0]["command"]
        assert "--show-docs true" in cmd

    def test_missing_base_raises(self, tmp_path: Path):
        with pytest.raises(BobbinSetupError, match="Base settings not found"):
            generate_override_settings(
                str(tmp_path / "nope.json"), {}, str(tmp_path / "out.json"),
            )


class TestSetupBobbinWithOverrides:
    @pytest.fixture()
    def mock_bobbin(self):
        popen_inst = MagicMock()
        popen_inst.stderr = iter([])
        popen_inst.stdout.read.return_value = ""
        popen_inst.communicate.return_value = ("", "")
        popen_inst.returncode = 0

        with (
            patch("runner.bobbin_setup._find_bobbin", return_value="/usr/bin/bobbin"),
            patch("runner.bobbin_setup.subprocess.run") as mock_run,
            patch("runner.bobbin_setup.subprocess.Popen") as mock_popen,
            patch("runner.bobbin_setup.os.killpg"),
        ):
            mock_run.return_value = subprocess.CompletedProcess(
                args=[], returncode=0, stdout="", stderr="",
            )
            mock_popen.return_value = popen_inst
            yield mock_run, mock_popen

    def test_applies_overrides_after_init(self, mock_bobbin, tmp_path: Path):
        """config_overrides are applied to config.toml created by init."""
        mock_run, _ = mock_bobbin

        # Pre-create the config file that bobbin init would produce.
        config_dir = tmp_path / ".bobbin"
        config_dir.mkdir(exist_ok=True)
        (config_dir / "config.toml").write_text(_SAMPLE_CONFIG)

        mock_run.side_effect = [
            subprocess.CompletedProcess(args=[], returncode=0, stdout="", stderr=""),  # init
            subprocess.CompletedProcess(args=[], returncode=0, stdout="{}", stderr=""),  # status
        ]

        setup_bobbin(str(tmp_path), config_overrides={"semantic_weight": "0.0"})

        content = (config_dir / "config.toml").read_text()
        assert "semantic_weight = 0.0" in content
